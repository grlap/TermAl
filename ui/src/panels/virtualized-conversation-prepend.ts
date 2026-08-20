// Owns history-prepend position reconciliation for the virtualized
// conversation list. It keeps the controller component focused on range and
// input orchestration while preserving anchor and boundary authority here.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.

import {
  useLayoutEffect,
  type MutableRefObject,
  type RefObject,
} from "react";
import type { Message } from "../types";
import { SESSION_STICKY_BOTTOM_BAND_PX } from "../scroll-position";
import {
  clampVirtualizedViewportScrollTop,
  isScrollContainerNearBottom,
} from "./conversation-virtualization";
import {
  PREPENDED_MESSAGE_ANCHOR_RESTORE_ATTEMPTS,
  rangesEqual,
  resolvePrependedMessageCount,
} from "./virtualized-conversation-controller";
import {
  captureFirstVisibleMountedMessageAnchor,
  estimateMessageOffsetWithinPage,
  findMountedMessageSlotById,
  findPageIndexContainingMessageBoundary,
  getMountedSlotViewportOffsetPx,
  type MessageLocation,
  type MessagePage,
  type PendingVisibleMessageAnchor,
  type VirtualizedRange,
  type VisibleMessageAnchor,
} from "./virtualized-conversation-measurement";
import type { MessageWindowSnapshot } from "./virtualized-conversation-types";

type MountedPrependRestore = {
  anchor: VisibleMessageAnchor | null;
  scrollHeight: number;
  scrollTop: number;
};

export function useVirtualizedConversationPrependEffects({
  applyMountedPageRange,
  buildWorkingMountedRangeForScrollTop,
  clearPendingDeferredLayoutTimer,
  estimateMessageHeight,
  hasUserScrollInteractionRef,
  isActive,
  isDetachedFromBottomRef,
  lastNativeScrollTopRef,
  latestVisibleMessageAnchorRef,
  layoutVersion,
  messageLocationById,
  messages,
  mountedPageRange,
  mountedPageRangeRef,
  pageLayout,
  pages,
  pendingMountedPrependRestoreRef,
  pendingPrependedBottomGapRef,
  pendingPrependedMessageAnchorRef,
  pendingPrependedTopBoundaryRef,
  previousMessageWindowRef,
  renderedListRef,
  scrollContainerRef,
  sessionId,
  shouldKeepBottomAfterLayoutRef,
  skipNextMountedPrependRestoreRef,
  viewportHeight,
  writeScrollTopAndSyncViewport,
}: {
  applyMountedPageRange: (
    nextRange: VirtualizedRange,
    options?: { flush?: boolean },
  ) => void;
  buildWorkingMountedRangeForScrollTop: (
    scrollTop: number,
    clientHeight: number,
  ) => VirtualizedRange;
  clearPendingDeferredLayoutTimer: () => void;
  estimateMessageHeight: (message: Message) => number;
  hasUserScrollInteractionRef: MutableRefObject<boolean>;
  isActive: boolean;
  isDetachedFromBottomRef: MutableRefObject<boolean>;
  lastNativeScrollTopRef: MutableRefObject<number>;
  latestVisibleMessageAnchorRef: MutableRefObject<VisibleMessageAnchor | null>;
  layoutVersion: number;
  messageLocationById: ReadonlyMap<string, MessageLocation>;
  messages: readonly Message[];
  mountedPageRange: VirtualizedRange;
  mountedPageRangeRef: MutableRefObject<VirtualizedRange>;
  pageLayout: { tops: readonly number[]; totalHeight: number };
  pages: readonly MessagePage[];
  pendingMountedPrependRestoreRef: MutableRefObject<MountedPrependRestore | null>;
  pendingPrependedBottomGapRef: MutableRefObject<number | null>;
  pendingPrependedMessageAnchorRef: MutableRefObject<PendingVisibleMessageAnchor | null>;
  pendingPrependedTopBoundaryRef: MutableRefObject<boolean>;
  previousMessageWindowRef: MutableRefObject<MessageWindowSnapshot>;
  renderedListRef: RefObject<HTMLDivElement | null>;
  scrollContainerRef: RefObject<HTMLElement | null>;
  sessionId: string;
  shouldKeepBottomAfterLayoutRef: MutableRefObject<boolean>;
  skipNextMountedPrependRestoreRef: MutableRefObject<boolean>;
  viewportHeight: number;
  writeScrollTopAndSyncViewport: (
    node: HTMLElement,
    nextScrollTop: number,
  ) => void;
}) {
  useLayoutEffect(() => {
    const previousWindow = previousMessageWindowRef.current;
    previousMessageWindowRef.current = {
      ids: messages.map((message) => message.id),
      sessionId,
    };

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

    // A bounded history window may begin midway through a global page. Locate
    // the prepend boundary from actual local page ranges, never page arithmetic.
    const prependedPageIndex = findPageIndexContainingMessageBoundary(
      pages,
      prependedCount,
    );
    const prependedPage = pages[prependedPageIndex];
    if (!prependedPage) {
      return;
    }

    const prependedOffsetWithinPage = estimateMessageOffsetWithinPage(
      prependedPage,
      prependedCount - prependedPage.startIndex,
      estimateMessageHeight,
    );
    if (prependedOffsetWithinPage === null) {
      return;
    }
    const prependedHeightPx =
      (pageLayout.tops[prependedPageIndex] ?? 0) + prependedOffsetWithinPage;
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
      ? findMountedMessageSlotById(
          renderedListRef.current,
          preservedAnchor.messageId,
        )
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
      pageLayout.totalHeight - (targetScrollTop + viewportHeightPx) <
      SESSION_STICKY_BOTTOM_BAND_PX;
    const preserveDetachedScroll =
      hasUserScrollInteractionRef.current ||
      isDetachedFromBottomRef.current ||
      !isScrollContainerNearBottom(node);
    shouldKeepBottomAfterLayoutRef.current =
      targetNearBottom && !preserveDetachedScroll;
    isDetachedFromBottomRef.current =
      preserveDetachedScroll || !targetNearBottom;
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
            endIndex: Math.min(
              preservedAnchorLocation.pageIndex + 5,
              pages.length,
            ),
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
        (anchorSlot.getBoundingClientRect().top -
          node.getBoundingClientRect().top) -
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
  }, [isActive, layoutVersion, mountedPageRange, scrollContainerRef]);
}
