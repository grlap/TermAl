// Owns measured page-height updates for the virtualized conversation list.
// Does not own page rendering, range scheduling, or scroll event listeners.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.
import {
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
import { useCommittedRef } from "./use-committed-ref";
import { useStableEvent } from "./use-stable-event";

const MESSAGE_SLOT_SELECTOR = ".virtualized-message-slot[data-message-id]";
const DEFERRED_CONTENT_PENDING_SELECTOR = '[data-deferred-content-pending="true"]';
const PROVENANCE_SCAN_SELECTOR =
  `${MESSAGE_SLOT_SELECTOR}, ${DEFERRED_CONTENT_PENDING_SELECTOR}`;

/**
 * Returns the ids of the page's message slots whose content is fully
 * rendered: every slot with a `data-message-id`, in document order, except a
 * slot with a pending deferred-content marker anywhere below it. One subtree
 * query finds slots and markers together; each marker then excludes every
 * slot it sits inside (not the slot it is on). When the ids equal `previous`,
 * `previous` itself is returned so repeated measurements allocate nothing.
 */
export function collectFullyRenderedMessageIds(
  pageNode: Element,
  previous?: readonly string[],
): readonly string[] {
  const slots: HTMLElement[] = [];
  let pendingSlots: Set<Element> | null = null;
  for (const element of pageNode.querySelectorAll<HTMLElement>(
    PROVENANCE_SCAN_SELECTOR,
  )) {
    if (element.matches(MESSAGE_SLOT_SELECTOR)) {
      slots.push(element);
    }
    if (!element.matches(DEFERRED_CONTENT_PENDING_SELECTOR)) {
      continue;
    }
    let ancestorSlot = element.parentElement?.closest(MESSAGE_SLOT_SELECTOR);
    while (
      ancestorSlot &&
      ancestorSlot !== pageNode &&
      pageNode.contains(ancestorSlot)
    ) {
      pendingSlots ??= new Set();
      pendingSlots.add(ancestorSlot);
      ancestorSlot = ancestorSlot.parentElement?.closest(MESSAGE_SLOT_SELECTOR);
    }
  }

  let ids: string[] | null = null;
  let count = 0;
  for (const slot of slots) {
    if (pendingSlots?.has(slot)) {
      continue;
    }
    const messageId = slot.dataset.messageId!;
    if (ids === null && previous?.[count] !== messageId) {
      ids = previous ? previous.slice(0, count) : [];
    }
    ids?.push(messageId);
    count += 1;
  }
  if (ids !== null) {
    return ids;
  }
  // Every id so far matched `previous`; it is reusable unless it is longer.
  if (previous !== undefined && previous.length === count) {
    return previous;
  }
  return (previous ?? []).slice(0, count);
}

export function shouldFlushVirtualizedPageHeightLayout({
  flushLayout,
  hasUserScrollInteraction,
  isInUserScrollCooldown,
  shouldKeepBottom,
}: {
  flushLayout: boolean;
  hasUserScrollInteraction: boolean;
  isInUserScrollCooldown: boolean;
  shouldKeepBottom: boolean;
}) {
  return (
    flushLayout &&
    shouldKeepBottom &&
    !hasUserScrollInteraction &&
    !isInUserScrollCooldown
  );
}

export function useVirtualizedConversationPageHeightChange({
  bumpLayoutVersion,
  clearPendingDeferredBottomRestore,
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
  scheduleDeferredBottomRestoreLayoutVersion,
  scheduleDeferredLayoutVersion,
  scrollContainerRef,
  shouldKeepBottomAfterLayoutRef,
  userScrollAdjustmentCooldownMs,
  visiblePageRangeRef,
  writeScrollTopAndSyncViewport,
}: {
  bumpLayoutVersion: () => void;
  clearPendingDeferredBottomRestore: () => void;
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
  scheduleDeferredBottomRestoreLayoutVersion: (delayMs: number) => void;
  scheduleDeferredLayoutVersion: (delayMs: number) => void;
  scrollContainerRef: RefObject<HTMLElement | null>;
  shouldKeepBottomAfterLayoutRef: MutableRefObject<boolean>;
  userScrollAdjustmentCooldownMs: number;
  visiblePageRangeRef: MutableRefObject<VirtualizedRange>;
  writeScrollTopAndSyncViewport: (node: HTMLElement, nextScrollTop: number) => void;
}) {
  const isActiveRef = useCommittedRef(isActive);
  const handlePageHeightChange = (
    pageKey: string,
    pageIndex: number,
    nextHeight: number,
    pageNode?: HTMLElement | null,
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
    // Provenance is recomputed on every measurement, equal height included:
    // placeholder activation can change it without changing geometry.
    const previousIdentity = measuredPageIdentityRef.current[pageKey];
    const fullyRenderedMessageIds = pageNode
      ? collectFullyRenderedMessageIds(
          pageNode,
          previousIdentity?.fullyRenderedMessageIds,
        )
      : undefined;
    const measuredIdentity: PageMeasurementIdentity =
      previousIdentity !== undefined &&
      previousIdentity.messages === currentPageIdentity.messages &&
      previousIdentity.hasTrailingGap === currentPageIdentity.hasTrailingGap &&
      previousIdentity.fullyRenderedMessageIds === fullyRenderedMessageIds
        ? previousIdentity
        : { ...currentPageIdentity, fullyRenderedMessageIds };
    const previousHeight = pageHeightsRef.current[pageKey];
    if (previousHeight !== undefined && Math.abs(previousHeight - roundedHeight) < 1) {
      measuredPageIdentityRef.current[pageKey] = measuredIdentity;
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
    measuredPageIdentityRef.current[pageKey] = measuredIdentity;
    layoutPageHeightsRef.current[pageKey] = roundedHeight;

    const node = scrollContainerRef.current;
    if (node && hasUserScrollInteractionRef.current && !isScrollContainerNearBottom(node)) {
      shouldKeepBottomAfterLayoutRef.current = false;
    }
    const isCurrentlyActive = isActiveRef.current;
    const shouldKeepBottom =
      isCurrentlyActive && node
        ? !isDetachedFromBottomRef.current &&
          (shouldKeepBottomAfterLayoutRef.current || isScrollContainerNearBottom(node))
        : false;
    if (shouldKeepBottom) {
      shouldKeepBottomAfterLayoutRef.current = true;
    } else {
      clearPendingDeferredBottomRestore();
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
      isCurrentlyActive &&
      !isMessagePrependCommitRef.current &&
      pageIndex <= latestVisiblePageRange.startIndex &&
      (hasUserScrollInteractionRef.current ||
        isDetachedFromBottomRef.current ||
        !isScrollContainerNearBottom(node))
    ) {
      const correction =
        anchorOffsetDelta !== null
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
        // ResizeObserver can report multiple page refinements in one delivery.
        // The matching native scroll event is queued only after this callback,
        // so keep the synchronous reference aligned with our anchor write.
        // Otherwise the first correction makes the second page look like an
        // unrelated scroll and leaves a small uncompensated viewport shift.
        lastNativeScrollTopRef.current = node.scrollTop;
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
        shouldFlushVirtualizedPageHeightLayout({
          flushLayout,
          hasUserScrollInteraction: hasUserScrollInteractionRef.current,
          isInUserScrollCooldown: isInUserScrollCooldown(),
          shouldKeepBottom,
        })
      ) {
        // ResizeObserver measurements arrive in an animation frame before
        // paint. Commit the refined spacer synchronously only when its paired
        // bottom restoration can run in the same frame. During the user-scroll
        // cooldown both parts stay asynchronous so inertial input keeps
        // authority over the viewport.
        flushSync(bumpLayoutVersion);
      } else {
        bumpLayoutVersion();
      }
      if (
        shouldKeepBottom &&
        !hasUserScrollInteractionRef.current &&
        isInUserScrollCooldown()
      ) {
        const remainingCooldown =
          userScrollAdjustmentCooldownMs -
          (performance.now() - lastUserScrollInputTimeRef.current);
        // Re-run the layout-owned bottom restoration once the gesture window
        // expires. This avoids both a permanent post-measurement gap and a
        // synchronous recapture between inertial native scroll ticks.
        scheduleDeferredBottomRestoreLayoutVersion(remainingCooldown);
        return;
      }
      clearPendingDeferredBottomRestore();
      restoreBottomAfterLayout();
      return;
    }

    const timeSinceUserScroll = performance.now() - lastUserScrollInputTimeRef.current;
    const inUserScrollCooldown = timeSinceUserScroll < userScrollAdjustmentCooldownMs;
    if (inUserScrollCooldown) {
      const remainingCooldown =
        userScrollAdjustmentCooldownMs - timeSinceUserScroll;
      if (shouldKeepBottom && !hasUserScrollInteractionRef.current) {
        scheduleDeferredBottomRestoreLayoutVersion(remainingCooldown);
      } else {
        scheduleDeferredLayoutVersion(remainingCooldown);
      }
      return;
    }

    scheduleDeferredLayoutVersion(0);
    restoreBottomAfterLayout();
  };

  return useStableEvent(handlePageHeightChange);
}
