// Owns native and programmatic scroll event orchestration for the virtualized
// conversation list.
// Does not own page measurement, rendering, or layout snapshot construction.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.
import { useLayoutEffect, type MutableRefObject, type RefObject } from "react";
import {
  canNestedScrollableConsumeWheel,
  normalizeWheelDelta,
} from "../app-utils";
import {
  MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS,
  MESSAGE_STACK_SCROLL_WRITE_EVENT,
  type MessageStackScrollWriteDetail,
} from "../message-stack-scroll-sync";
import {
  DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
  getScrollContainerBottomGap,
  isScrollContainerNearBottom,
} from "./conversation-virtualization";
import {
  captureFirstVisibleMountedMessageAnchor,
  type PendingVisibleMessageAnchor,
  type VirtualizedRange,
  type VisibleMessageAnchor,
} from "./virtualized-conversation-measurement";
import type { UserScrollKind } from "./virtualized-conversation-types";

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

function classifyScrollKind(
  scrollDelta: number,
  clientHeight: number,
): Exclude<UserScrollKind, null> {
  return Math.abs(scrollDelta) >=
    Math.max(clientHeight * 1.5, DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT)
    ? "seek"
    : "incremental";
}

type MountedPrependRestore = {
  anchor: VisibleMessageAnchor | null;
  scrollHeight: number;
  scrollTop: number;
};

type DeferredLayoutAnchor = {
  messageId: string;
  viewportOffsetPx: number;
};

export function useVirtualizedConversationScrollEvents({
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
  userScrollAdjustmentCooldownMs,
}: {
  applyMountedPageRange: (nextRange: VirtualizedRange, options?: { flush?: boolean }) => void;
  buildBottomMountedRange: (clientHeight: number) => VirtualizedRange;
  cancelPostActivationBottomRestore: () => void;
  captureLatestVisibleMessageAnchor: (node: HTMLElement) => VisibleMessageAnchor | null;
  clearPendingDeferredLayoutTimer: () => void;
  clearPendingIdleCompactionTimer: () => void;
  hasUserScrollInteractionRef: MutableRefObject<boolean>;
  isActive: boolean;
  isDetachedFromBottomRef: MutableRefObject<boolean>;
  isMeasuringPostActivation: boolean;
  lastNativeScrollTopRef: MutableRefObject<number>;
  lastTouchClientYRef: MutableRefObject<number | null>;
  lastUserScrollInputTimeRef: MutableRefObject<number>;
  lastUserScrollKindRef: MutableRefObject<UserScrollKind>;
  latestVisibleMessageAnchorRef: MutableRefObject<VisibleMessageAnchor | null>;
  mountBottomBoundary: (node: HTMLElement) => void;
  pendingAggressiveIdleCompactionRef: MutableRefObject<boolean>;
  pendingBottomBoundaryRevealNodeRef: MutableRefObject<HTMLElement | null>;
  pendingBottomBoundarySeekRef: MutableRefObject<boolean>;
  pendingDeferredLayoutAnchorRef: MutableRefObject<DeferredLayoutAnchor | null>;
  pendingMountedPrependRestoreRef: MutableRefObject<MountedPrependRestore | null>;
  pendingPrependedBottomGapRef: MutableRefObject<number | null>;
  pendingPrependedTopBoundaryRef: MutableRefObject<boolean>;
  pendingProgrammaticBottomFollowUntilRef: MutableRefObject<number>;
  pendingProgrammaticScrollTopRef: MutableRefObject<number | null>;
  prewarmMountedRangeForUpwardWheel: (node: HTMLElement, wheelDeltaY: number) => void;
  reconcileMountedRangeForNativeScroll: (
    node: HTMLElement,
    scrollDelta: number,
    scrollKind: UserScrollKind,
    options?: { allowSeekFlush?: boolean; flush?: boolean },
  ) => void;
  releaseConversationSearchPinForUserScroll: () => void;
  renderedListRef: RefObject<HTMLDivElement | null>;
  scheduleBottomBoundaryReveal: (node: HTMLElement) => void;
  scheduleIdleMountedRangeCompaction: (delayMs: number) => void;
  scheduleProgrammaticViewportSync: (node: HTMLElement) => void;
  scrollContainerRef: RefObject<HTMLElement | null>;
  setHasUserScrollInteraction: (nextValue: boolean) => void;
  shouldKeepBottomAfterLayoutRef: MutableRefObject<boolean>;
  skipNextMountedPrependRestoreRef: MutableRefObject<boolean>;
  suspendDeferredRenderActivation: (node: HTMLElement) => void;
  syncViewportFromScrollNode: (node: HTMLElement) => void;
  tailFollowIntent: boolean;
  userScrollAdjustmentCooldownMs: number;
}) {
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
            scheduleIdleMountedRangeCompaction(userScrollAdjustmentCooldownMs);
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
      if (upwardInputDeltaPx === null || !isLikelyBottomEscape) {
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
      scheduleIdleMountedRangeCompaction(userScrollAdjustmentCooldownMs);
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
            : classifyScrollKind(scrollDelta, node.clientHeight));
        const scrollWriteTime = performance.now();
        lastUserScrollKindRef.current = isNearBottomAfterWrite
          ? null
          : resolvedScrollKind;
        if (!isNearBottomAfterWrite) {
          if (explicitScrollSource === "user") {
            releaseConversationSearchPinForUserScroll();
          }
          lastUserScrollInputTimeRef.current = scrollWriteTime;
          scheduleIdleMountedRangeCompaction(userScrollAdjustmentCooldownMs);
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
    const ResizeObserverCtor = globalThis.ResizeObserver;
    const resizeObserver =
      typeof ResizeObserverCtor === "function"
        ? new ResizeObserverCtor(() => {
            syncViewport();
          })
        : null;
    resizeObserver?.observe(node);

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
      resizeObserver?.disconnect();
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
    reconcileMountedRangeForNativeScroll,
    releaseConversationSearchPinForUserScroll,
    scheduleBottomBoundaryReveal,
    scheduleIdleMountedRangeCompaction,
    scheduleProgrammaticViewportSync,
    scrollContainerRef,
    setHasUserScrollInteraction,
    suspendDeferredRenderActivation,
    syncViewportFromScrollNode,
    tailFollowIntent,
    userScrollAdjustmentCooldownMs,
  ]);
}
