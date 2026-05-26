// Owns imperative jump and layout-snapshot handles for the virtualized
// conversation list.
// Does not own rendered page bands, native scroll events, or page measurement.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.
import {
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  type MutableRefObject,
  type RefObject,
} from "react";
import {
  VIRTUALIZED_MESSAGE_GAP_PX,
  clampVirtualizedViewportScrollTop,
} from "./conversation-virtualization";
import {
  VIRTUALIZED_MESSAGES_PER_PAGE,
  estimateMessageOffsetWithinPage,
  findMountedMessageSlotById,
  type MessageLocation,
  type MessagePage,
  type VirtualizedRange,
  type VisibleMessageAnchor,
} from "./virtualized-conversation-measurement";
import type {
  VirtualizedConversationJumpOptions,
  VirtualizedConversationLayoutMessage,
  VirtualizedConversationLayoutSnapshot,
  VirtualizedConversationMessageListHandle,
  VirtualizedConversationMessageListHandleRef,
  VirtualizedConversationViewportSnapshot,
  UserScrollKind,
} from "./virtualized-conversation-types";
import type { Message } from "../types";

type PageLayout = {
  tops: readonly number[];
  totalHeight: number;
};

type DeferredLayoutAnchor = {
  messageId: string;
  viewportOffsetPx: number;
};

type MountedPrependRestore = {
  anchor: VisibleMessageAnchor | null;
  scrollHeight: number;
  scrollTop: number;
};

type VirtualizerHandleState = {
  buildLayoutSnapshot: () => VirtualizedConversationLayoutSnapshot;
  buildViewportSnapshot: () => VirtualizedConversationViewportSnapshot;
  jumpToMessageLocation: (
    location: MessageLocation,
    options?: VirtualizedConversationJumpOptions,
  ) => boolean;
  messageLocationById: Map<string, MessageLocation>;
  messagesLength: number;
  pages: MessagePage[];
};

export function useVirtualizedConversationHandle({
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
}: {
  applyMountedPageRange: (nextRange: VirtualizedRange, options?: { flush?: boolean }) => void;
  buildWorkingMountedRangeForScrollTop: (
    scrollTop: number,
    viewportHeightPx: number,
  ) => VirtualizedRange;
  clearPendingDeferredLayoutTimer: () => void;
  clearPendingIdleCompactionTimer: () => void;
  estimateMessageHeight: (message: Message) => number;
  isActive: boolean;
  isDetachedFromBottomRef: MutableRefObject<boolean>;
  lastUserScrollInputTimeRef: MutableRefObject<number>;
  lastUserScrollKindRef: MutableRefObject<UserScrollKind>;
  messageLocationById: Map<string, MessageLocation>;
  messages: Message[];
  mountedPageRangeRef: MutableRefObject<VirtualizedRange>;
  pageHeightsRef: MutableRefObject<Record<string, number>>;
  pageLayout: PageLayout;
  pages: MessagePage[];
  pendingAggressiveIdleCompactionRef: MutableRefObject<boolean>;
  pendingDeferredLayoutAnchorRef: MutableRefObject<DeferredLayoutAnchor | null>;
  pendingMountedPrependRestoreRef: MutableRefObject<MountedPrependRestore | null>;
  pendingProgrammaticBottomFollowUntilRef: MutableRefObject<number>;
  pendingProgrammaticScrollTopRef: MutableRefObject<number | null>;
  renderedListRef: RefObject<HTMLDivElement | null>;
  renderedMountedPageRange: VirtualizedRange;
  scrollContainerRef: RefObject<HTMLElement | null>;
  sessionId: string;
  setHasUserScrollInteraction: (nextValue: boolean) => void;
  shouldKeepBottomAfterLayoutRef: MutableRefObject<boolean>;
  skipNextMountedPrependRestoreRef: MutableRefObject<boolean>;
  viewportHeight: number;
  viewportScrollTop: number;
  viewportWidth: number;
  virtualizerHandleRef?: VirtualizedConversationMessageListHandleRef;
  visiblePageRange: VirtualizedRange;
  writeScrollTopAndSyncViewport: (node: HTMLElement, nextScrollTop: number) => void;
}) {
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
      if (nextMountedRange.startIndex !== mountedPageRangeRef.current.startIndex ||
        nextMountedRange.endIndex !== mountedPageRangeRef.current.endIndex) {
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
      messages,
      pageLayout.totalHeight,
      renderedMountedPageRange,
      scrollContainerRef,
      sessionId,
      viewportHeight,
      viewportScrollTop,
      viewportWidth,
      visiblePageRange,
    ]);

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
}
