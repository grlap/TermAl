// Owns measured page-height updates for the virtualized conversation list.
// Does not own page rendering, range scheduling, or scroll event listeners.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.
import {
  useCallback,
  useRef,
  type MutableRefObject,
  type RefObject,
} from "react";
import { flushSync } from "react-dom";
import { requestMessageStackBottomRepin } from "../message-stack-scroll-sync";
import { isScrollContainerNearBottom } from "./conversation-virtualization";
import {
  doesMountedPageIntersectViewport,
  findMountedMessageSlotById,
  getMountedSlotViewportOffsetPx,
  type PageMeasurementIdentity,
  type VirtualizedRange,
  type VisibleMessageAnchor,
} from "./virtualized-conversation-measurement";

export function useVirtualizedConversationPageHeightChange({
  bumpLayoutVersion,
  clearPendingDeferredLayoutTimer,
  hasUserScrollInteractionRef,
  isActive,
  isDetachedFromBottomRef,
  isMessagePrependCommitRef,
  lastNativeScrollTopRef,
  lastUserScrollInputTimeRef,
  latestVisibleMessageAnchorRef,
  layoutPageHeightsRef,
  measuredPageIdentityRef,
  pageHeightsRef,
  currentPageIdentityRef,
  renderedListRef,
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
  isMessagePrependCommitRef: MutableRefObject<boolean>;
  lastNativeScrollTopRef: MutableRefObject<number>;
  lastUserScrollInputTimeRef: MutableRefObject<number>;
  latestVisibleMessageAnchorRef: MutableRefObject<VisibleMessageAnchor | null>;
  layoutPageHeightsRef: MutableRefObject<Record<string, number>>;
  measuredPageIdentityRef: MutableRefObject<
    Record<string, PageMeasurementIdentity>
  >;
  pageHeightsRef: MutableRefObject<Record<string, number>>;
  currentPageIdentityRef: MutableRefObject<
    Record<string, PageMeasurementIdentity>
  >;
  renderedListRef: RefObject<HTMLDivElement | null>;
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
      flushLayout?: boolean,
    ) => void) | null
  >(null);

  handlePageHeightChangeRef.current = (
    pageKey,
    pageIndex,
    nextHeight,
    pageNode,
    flushLayout = false,
  ) => {
    if (!Number.isFinite(nextHeight) || nextHeight <= 0) {
      return;
    }

    const roundedHeight = Math.round(nextHeight);
    const currentPageIdentity = currentPageIdentityRef.current[pageKey];
    if (!currentPageIdentity) {
      // A disconnected ResizeObserver callback from a replaced page must not
      // reintroduce its height under a key now owned by different content.
      return;
    }
    const previousHeight = pageHeightsRef.current[pageKey];
    if (previousHeight !== undefined && Math.abs(previousHeight - roundedHeight) < 1) {
      measuredPageIdentityRef.current[pageKey] = currentPageIdentity;
      return;
    }

    const previousLayoutHeight =
      previousHeight ?? layoutPageHeightsRef.current[pageKey];
    const layoutHeightDelta =
      previousLayoutHeight !== undefined &&
      Number.isFinite(previousLayoutHeight)
        ? roundedHeight - previousLayoutHeight
        : 0;
    pageHeightsRef.current[pageKey] = roundedHeight;
    measuredPageIdentityRef.current[pageKey] = currentPageIdentity;
    layoutPageHeightsRef.current[pageKey] = roundedHeight;

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
        if (!requestMessageStackBottomRepin(node)) {
          const target = Math.max(node.scrollHeight - node.clientHeight, 0);
          writeScrollTopAndSyncViewport(node, target);
        }
      });
    };

    const latestVisiblePageRange = visiblePageRangeRef.current;
    // A mounted page at or above the viewport can finish measuring well after a
    // bounded history prepend first restores the visible message anchor.
    // Compare the anchor's live DOM offset with the offset captured by the
    // scroll path; this handles both whole pages above the viewport and growth
    // above the anchor inside the first visible page. Fall back to the page
    // height delta only when that anchor is not mounted. The custom virtualizer
    // owns this correction, so CSS disables native overflow anchoring.
    const preservedAnchor = latestVisibleMessageAnchorRef.current;
    const preservedAnchorSlot = preservedAnchor
      ? findMountedMessageSlotById(
          renderedListRef.current,
          preservedAnchor.messageId,
        )
      : null;
    const hasSyncedNativeScrollTop =
      node !== null &&
      Math.abs(node.scrollTop - lastNativeScrollTopRef.current) <= 1;
    const anchorOffsetDelta =
      node && hasSyncedNativeScrollTop && preservedAnchor && preservedAnchorSlot
        ? getMountedSlotViewportOffsetPx(node, preservedAnchorSlot) -
          preservedAnchor.viewportOffsetPx
        : null;
    if (
      node &&
      hasSyncedNativeScrollTop &&
      isActive &&
      !isMessagePrependCommitRef.current &&
      pageIndex <= latestVisiblePageRange.startIndex &&
      (hasUserScrollInteractionRef.current ||
        isDetachedFromBottomRef.current ||
        !isScrollContainerNearBottom(node))
    ) {
      const correction =
        anchorOffsetDelta !== null && Math.abs(anchorOffsetDelta) >= 1
          ? anchorOffsetDelta
          : pageIndex < latestVisiblePageRange.startIndex &&
              Math.abs(layoutHeightDelta) >= 1
            ? layoutHeightDelta
            : 0;
      if (Math.abs(correction) >= 1) {
        writeScrollTopAndSyncViewport(
          node,
          Math.max(node.scrollTop + correction, 0),
        );
      }
    }
    const isVisiblePage =
      pageIndex >= latestVisiblePageRange.startIndex &&
      pageIndex < latestVisiblePageRange.endIndex;
    const intersectsViewport =
      node !== null && doesMountedPageIntersectViewport(node, pageNode);
    if (isVisiblePage || intersectsViewport) {
      clearPendingDeferredLayoutTimer();
      if (
        flushLayout &&
        shouldKeepBottom &&
        !hasUserScrollInteractionRef.current &&
        !isInUserScrollCooldown()
      ) {
        // ResizeObserver measurements arrive in an animation frame before
        // paint. Commit the refined spacer and the layout-effect bottom write
        // in that same frame; exposing the new spacer for one paint first
        // creates a visible blank gap followed by an anchor jump.
        flushSync(bumpLayoutVersion);
      } else {
        bumpLayoutVersion();
      }
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
      flushLayout?: boolean,
    ) => {
      handlePageHeightChangeRef.current?.(
        pageKey,
        pageIndex,
        nextHeight,
        pageNode,
        flushLayout,
      );
    },
    [],
  );
}
