// Owns measured page-height updates for the virtualized conversation list.
// Does not own page rendering, range scheduling, or scroll event listeners.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.
import {
  useCallback,
  useRef,
  type MutableRefObject,
  type RefObject,
} from "react";
import { isScrollContainerNearBottom } from "./conversation-virtualization";
import {
  doesMountedPageIntersectViewport,
  type VirtualizedRange,
} from "./virtualized-conversation-measurement";

export function useVirtualizedConversationPageHeightChange({
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
  userScrollAdjustmentCooldownMs,
  visiblePageRangeRef,
  writeScrollTopAndSyncViewport,
}: {
  bumpLayoutVersion: () => void;
  clearPendingDeferredLayoutTimer: () => void;
  hasUserScrollInteractionRef: MutableRefObject<boolean>;
  isActive: boolean;
  isDetachedFromBottomRef: MutableRefObject<boolean>;
  lastUserScrollInputTimeRef: MutableRefObject<number>;
  pageHeightsRef: MutableRefObject<Record<string, number>>;
  scheduleDeferredLayoutVersion: (delayMs: number) => void;
  scrollContainerRef: RefObject<HTMLElement | null>;
  shouldKeepBottomAfterLayoutRef: MutableRefObject<boolean>;
  userScrollAdjustmentCooldownMs: number;
  visiblePageRangeRef: MutableRefObject<VirtualizedRange>;
  writeScrollTopAndSyncViewport: (node: HTMLElement, nextScrollTop: number) => void;
}) {
  const handlePageHeightChangeRef = useRef<
    ((
      pageKey: string,
      pageIndex: number,
      nextHeight: number,
      pageNode?: HTMLElement | null,
    ) => void) | null
  >(null);

  handlePageHeightChangeRef.current = (pageKey, pageIndex, nextHeight, pageNode) => {
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

    const isInUserScrollCooldown = () =>
      performance.now() - lastUserScrollInputTimeRef.current <
      userScrollAdjustmentCooldownMs;

    const restoreBottomAfterLayout = () => {
      if (
        !node ||
        !shouldKeepBottom ||
        hasUserScrollInteractionRef.current ||
        isInUserScrollCooldown()
      ) {
        return;
      }
      window.requestAnimationFrame(() => {
        if (scrollContainerRef.current !== node) {
          return;
        }
        if (
          hasUserScrollInteractionRef.current ||
          isDetachedFromBottomRef.current ||
          !shouldKeepBottomAfterLayoutRef.current ||
          isInUserScrollCooldown()
        ) {
          return;
        }
        const target = Math.max(node.scrollHeight - node.clientHeight, 0);
        writeScrollTopAndSyncViewport(node, target);
      });
    };

    const latestVisiblePageRange = visiblePageRangeRef.current;
    const isVisiblePage =
      pageIndex >= latestVisiblePageRange.startIndex &&
      pageIndex < latestVisiblePageRange.endIndex;
    const intersectsViewport =
      node !== null && doesMountedPageIntersectViewport(node, pageNode);
    if (isVisiblePage || intersectsViewport) {
      clearPendingDeferredLayoutTimer();
      bumpLayoutVersion();
      restoreBottomAfterLayout();
      return;
    }

    const timeSinceUserScroll = performance.now() - lastUserScrollInputTimeRef.current;
    const inUserScrollCooldown = timeSinceUserScroll < userScrollAdjustmentCooldownMs;
    if (inUserScrollCooldown) {
      scheduleDeferredLayoutVersion(
        userScrollAdjustmentCooldownMs - timeSinceUserScroll,
      );
      return;
    }

    scheduleDeferredLayoutVersion(0);
    restoreBottomAfterLayout();
  };

  return useCallback(
    (
      pageKey: string,
      pageIndex: number,
      nextHeight: number,
      pageNode?: HTMLElement | null,
    ) => {
      handlePageHeightChangeRef.current?.(pageKey, pageIndex, nextHeight, pageNode);
    },
    [],
  );
}
