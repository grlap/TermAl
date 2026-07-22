// Owns mounted-page range growth, compaction, and spacer-coverage effects for
// the virtualized conversation list.
// Does not own native input listeners, page measurement, or message rendering.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.
import { useLayoutEffect, type MutableRefObject, type RefObject } from "react";
import { isScrollContainerNearBottom } from "./conversation-virtualization";
import {
  resolvePageCoverageHeight,
  resolveRenderedPageCoverageHeight,
  type PendingVisibleMessageAnchor,
  type VirtualizedRange,
  type VisibleMessageAnchor,
} from "./virtualized-conversation-measurement";
import type { UserScrollKind } from "./virtualized-conversation-types";

const IDLE_MOUNTED_COMPACTION_PAGE_HYSTERESIS = 2;

type MountedPrependRestore = {
  anchor: VisibleMessageAnchor | null;
  scrollHeight: number;
  scrollTop: number;
};

function rangesEqual(first: VirtualizedRange, second: VirtualizedRange) {
  return first.startIndex === second.startIndex && first.endIndex === second.endIndex;
}

function rangeContainsRange(container: VirtualizedRange, target: VirtualizedRange) {
  return (
    container.startIndex <= target.startIndex &&
    container.endIndex >= target.endIndex
  );
}

export function useVirtualizedConversationMountedRangeEffects({
  applyMountedPageRange,
  captureMountedPrependRestore,
  hasUserScrollInteractionRef,
  isActive,
  lastUserScrollInputTimeRef,
  lastUserScrollKindRef,
  layoutVersion,
  mountedPageRange,
  pageHeights,
  pagesLength,
  pendingAggressiveIdleCompactionRef,
  pendingIdleCompactionTimerRef,
  pendingMountedPrependRestoreRef,
  pendingPrependedMessageAnchorRef,
  pendingProgrammaticBottomFollowUntilRef,
  renderedListRef,
  scrollContainerRef,
  scrollIdleVersion,
  searchPinnedMountedPageRange,
  userScrollAdjustmentCooldownMs,
  viewportScrollTop,
  visiblePageRange,
  workingMountedPageRange,
}: {
  applyMountedPageRange: (nextRange: VirtualizedRange, options?: { flush?: boolean }) => void;
  captureMountedPrependRestore: (node: HTMLElement) => MountedPrependRestore;
  hasUserScrollInteractionRef: MutableRefObject<boolean>;
  isActive: boolean;
  lastUserScrollInputTimeRef: MutableRefObject<number>;
  lastUserScrollKindRef: MutableRefObject<UserScrollKind>;
  layoutVersion: number;
  mountedPageRange: VirtualizedRange;
  pageHeights: number[];
  pagesLength: number;
  pendingAggressiveIdleCompactionRef: MutableRefObject<boolean>;
  pendingIdleCompactionTimerRef: MutableRefObject<number | null>;
  pendingMountedPrependRestoreRef: MutableRefObject<MountedPrependRestore | null>;
  pendingPrependedMessageAnchorRef: MutableRefObject<PendingVisibleMessageAnchor | null>;
  pendingProgrammaticBottomFollowUntilRef: MutableRefObject<number>;
  renderedListRef: RefObject<HTMLDivElement | null>;
  scrollContainerRef: RefObject<HTMLElement | null>;
  scrollIdleVersion: number;
  searchPinnedMountedPageRange: VirtualizedRange | null;
  userScrollAdjustmentCooldownMs: number;
  viewportScrollTop: number;
  visiblePageRange: VirtualizedRange;
  workingMountedPageRange: VirtualizedRange;
}) {
  useLayoutEffect(() => {
    if (!isActive || pendingPrependedMessageAnchorRef.current) {
      return;
    }

    if (pagesLength === 0) {
      return;
    }

    const inUserScrollCooldown =
      performance.now() - lastUserScrollInputTimeRef.current < userScrollAdjustmentCooldownMs;
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
    pagesLength,
    scrollContainerRef,
    userScrollAdjustmentCooldownMs,
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

    // The scheduled idle transition is authoritative for shrinking the band.
    // A synchronous prewarm render can itself exceed the nominal cooldown on
    // a busy machine; elapsed wall time would then collapse the just-mounted
    // range before the input handler returns and before the browser can paint.
    const inUserScrollCooldown = pendingIdleCompactionTimerRef.current !== null;
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
    pendingIdleCompactionTimerRef,
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
        userScrollAdjustmentCooldownMs;
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
    userScrollAdjustmentCooldownMs,
    viewportScrollTop,
  ]);

  useLayoutEffect(() => {
    if (
      !isActive ||
      searchPinnedMountedPageRange !== null ||
      mountedPageRange.endIndex >= pagesLength
    ) {
      return;
    }

    const isUserScrollCooldown =
      hasUserScrollInteractionRef.current &&
      performance.now() - lastUserScrollInputTimeRef.current <
        userScrollAdjustmentCooldownMs;
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
    while (missingBelowPx > 0 && nextEndIndex < pagesLength) {
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
    pagesLength,
    scrollContainerRef,
    searchPinnedMountedPageRange,
    userScrollAdjustmentCooldownMs,
    viewportScrollTop,
  ]);
}
