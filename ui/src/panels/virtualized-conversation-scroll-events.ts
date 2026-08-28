// Owns native and programmatic scroll event orchestration for the virtualized
// conversation list.
// Does not own page measurement, rendering, or layout snapshot construction.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.
// Keyboard authority arrives as MESSAGE_STACK_USER_SCROLL_INTENT_EVENT from the
// pane host; this hook owns wheel/touch/native-scroll observation on the node.
import { useLayoutEffect, type MutableRefObject, type RefObject } from "react";
import { SESSION_STICKY_BOTTOM_BAND_PX } from "../scroll-position";
import {
  MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS,
  MESSAGE_STACK_POINTER_OWNERSHIP_MS,
  MESSAGE_STACK_SCROLL_WRITE_EVENT,
  MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
  MESSAGE_STACK_WHEEL_OWNERSHIP_MS,
  claimMessageStackNativeScrollOwnership,
  clearMessageStackNativeScrollOwnership,
  isMessageStackWheelEventSuppressed,
  messageStackNativeScrollOwnershipMovesTowardBottom,
  observeMessageStackPointerOwnershipRelease,
  peekMessageStackNativeScrollOwnership,
  resolveMessageStackWheelRouting,
  revokeMessageStackNativeScrollOwnershipOnConflict,
  type MessageStackScrollWriteDetail,
  type MessageStackUserScrollIntentDetail,
} from "../message-stack-scroll-sync";
import {
  DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
  getScrollContainerBottomGap,
  isScrollContainerAtPhysicalBottom,
  isScrollContainerNearBottom,
} from "./conversation-virtualization";
import {
  captureFirstVisibleMountedMessageAnchor,
  type PendingVisibleMessageAnchor,
  type VirtualizedRange,
  type VisibleMessageAnchor,
} from "./virtualized-conversation-measurement";
import type { MountedPrependRestore } from "./virtualized-conversation-mounted-range";
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

export function resolveStableHeightNativeUserMovement({
  currentScrollHeight,
  previousScrollHeight,
  scrollDelta,
}: {
  currentScrollHeight: number;
  previousScrollHeight: number;
  scrollDelta: number;
}) {
  return (
    Math.abs(scrollDelta) >= 0.5 &&
    Math.abs(currentScrollHeight - previousScrollHeight) < 1
  );
}

export function nativeScrollKeepsPassiveTailFollow({
  hadUserScrollInteraction,
  isDetachedFromBottom,
  isNativeUserMovement,
  isProgrammaticNavigation,
  scrollDelta,
  scrollHeightDelta,
  tailFollowIntent,
}: {
  hadUserScrollInteraction: boolean;
  isDetachedFromBottom: boolean;
  isNativeUserMovement: boolean;
  isProgrammaticNavigation: boolean;
  scrollDelta: number;
  scrollHeightDelta: number;
  tailFollowIntent: boolean;
}) {
  return (
    tailFollowIntent &&
    !hadUserScrollInteraction &&
    !isDetachedFromBottom &&
    (isProgrammaticNavigation ||
      scrollDelta >= 0 ||
      (!isNativeUserMovement && scrollHeightDelta < 0))
  );
}

type DeferredLayoutAnchor = {
  messageId: string;
  viewportOffsetPx: number;
};

export type PendingPrependNativeReflow = {
  expectedScrollHeight: number;
  expectedScrollTop: number;
  userScrollGeneration: number;
};

export function pendingPrependNativeReflowMatches({
  currentUserScrollGeneration,
  node,
  token,
}: {
  currentUserScrollGeneration: number;
  node: HTMLElement;
  token: PendingPrependNativeReflow | null;
}) {
  return Boolean(
    token &&
      token.userScrollGeneration === currentUserScrollGeneration &&
      Math.abs(node.scrollHeight - token.expectedScrollHeight) < 1 &&
      Math.abs(node.scrollTop - token.expectedScrollTop) < 1,
  );
}

export function nativeScrollAdvancesUserScrollGeneration({
  isExpectedPrependNativeReflow,
  isNativeUserMovement,
  isProgrammaticNavigation,
}: {
  isExpectedPrependNativeReflow: boolean;
  isNativeUserMovement: boolean;
  isProgrammaticNavigation: boolean;
}) {
  return (
    isNativeUserMovement &&
    !isProgrammaticNavigation &&
    !isExpectedPrependNativeReflow
  );
}

export function resolveVirtualizedInputMovementAuthority({
  bottomGapBeforeInput,
  explicitViewportCanMove,
  inputScrollDeltaY,
  isDetachedFromBottom,
  scrollTop,
  shouldKeepBottom,
}: {
  bottomGapBeforeInput: number;
  explicitViewportCanMove?: boolean;
  inputScrollDeltaY: number | null;
  isDetachedFromBottom: boolean;
  scrollTop: number;
  shouldKeepBottom: boolean;
}) {
  const inputCanMoveViewport =
    explicitViewportCanMove !== undefined
      ? explicitViewportCanMove
      : inputScrollDeltaY !== null &&
        (inputScrollDeltaY < 0
          ? scrollTop > 0.5
          : inputScrollDeltaY > 0
            ? bottomGapBeforeInput > 0.5
            : false);
  const isAttachedDownwardBoundaryInput =
    inputScrollDeltaY !== null &&
    inputScrollDeltaY > 0 &&
    !inputCanMoveViewport &&
    bottomGapBeforeInput <= 0.5 &&
    !isDetachedFromBottom &&
    shouldKeepBottom;
  return {
    inputCanMoveViewport,
    invalidatesPrependAuthority: inputCanMoveViewport,
    isAttachedDownwardBoundaryInput,
  };
}

export function useVirtualizedConversationScrollEvents({
  applyMountedPageRange,
  advanceUserScrollGeneration,
  buildBottomMountedRange,
  cancelPostActivationBottomRestore,
  captureLatestVisibleMessageAnchor,
  clearPendingDeferredBottomRestore,
  clearPendingDeferredLayoutTimer,
  clearPendingIdleCompactionTimer,
  getUserScrollGeneration,
  hasUserScrollInteractionRef,
  isActive,
  isDetachedFromBottomRef,
  isMeasuringPostActivation,
  lastNativeScrollTopRef,
  lastNativeScrollHeightRef,
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
  pendingPrependNativeReflowRef,
  pendingPrependedBottomGapRef,
  pendingPrependedTopBoundaryRef,
  pendingProgrammaticBottomFollowUntilRef,
  pendingProgrammaticNavigationUntilRef,
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
  applyMountedPageRange: (
    nextRange: VirtualizedRange,
    options?: { flush?: boolean; preserveCoveringRange?: boolean },
  ) => void;
  advanceUserScrollGeneration: () => void;
  buildBottomMountedRange: (clientHeight: number) => VirtualizedRange;
  cancelPostActivationBottomRestore: () => void;
  captureLatestVisibleMessageAnchor: (
    node: HTMLElement,
  ) => VisibleMessageAnchor | null;
  clearPendingDeferredBottomRestore: () => void;
  clearPendingDeferredLayoutTimer: () => void;
  clearPendingIdleCompactionTimer: () => void;
  getUserScrollGeneration: () => number;
  hasUserScrollInteractionRef: MutableRefObject<boolean>;
  isActive: boolean;
  isDetachedFromBottomRef: MutableRefObject<boolean>;
  isMeasuringPostActivation: boolean;
  lastNativeScrollTopRef: MutableRefObject<number>;
  lastNativeScrollHeightRef: MutableRefObject<number>;
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
  pendingPrependNativeReflowRef: MutableRefObject<PendingPrependNativeReflow | null>;
  pendingPrependedBottomGapRef: MutableRefObject<number | null>;
  pendingPrependedTopBoundaryRef: MutableRefObject<boolean>;
  pendingProgrammaticBottomFollowUntilRef: MutableRefObject<number>;
  pendingProgrammaticNavigationUntilRef: MutableRefObject<number>;
  pendingProgrammaticScrollTopRef: MutableRefObject<number | null>;
  prewarmMountedRangeForUpwardWheel: (
    node: HTMLElement,
    wheelDeltaY: number,
  ) => void;
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
    const stopObservingPointerRelease =
      observeMessageStackPointerOwnershipRelease(node);
    return () => {
      stopObservingPointerRelease();
      clearMessageStackNativeScrollOwnership(node);
    };
  }, [isActive, scrollContainerRef]);

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
      pendingProgrammaticNavigationUntilRef.current = Number.NEGATIVE_INFINITY;
      lastNativeScrollTopRef.current = node.scrollTop;
      shouldKeepBottomAfterLayoutRef.current = true;
      isDetachedFromBottomRef.current = false;
      setHasUserScrollInteraction(false);
      lastUserScrollKindRef.current = null;
      lastUserScrollInputTimeRef.current = Number.NEGATIVE_INFINITY;
    };
    const cancelBottomBoundaryRestore = () => {
      // A boundary command can arm its ref and reveal loop synchronously before
      // React publishes the measuring state. Newer user input must invalidate
      // both owners without relying on that render-time boolean.
      pendingBottomBoundarySeekRef.current = false;
      cancelPostActivationBottomRestore();
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
        const pendingPrependNativeReflow =
          pendingPrependNativeReflowRef.current;
        pendingPrependNativeReflowRef.current = null;
        const isExpectedPrependNativeReflow =
          pendingPrependNativeReflowMatches({
            currentUserScrollGeneration: getUserScrollGeneration(),
            node,
            token: pendingPrependNativeReflow,
          });
        const previousNativeScrollHeight = lastNativeScrollHeightRef.current;
        lastNativeScrollHeightRef.current = node.scrollHeight;
        const pendingProgrammaticScrollTop =
          pendingProgrammaticScrollTopRef.current;
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
          // This is the shared generic layout timer used by prepend, anchor,
          // and range restoration, so real native movement invalidates it. The
          // page-measurement bottom retry has a dedicated timer and remains
          // separately governed by bottom authority.
          clearPendingDeferredLayoutTimer();
          pendingDeferredLayoutAnchorRef.current = null;
          const scrollDelta = node.scrollTop - lastNativeScrollTopRef.current;
          const scrollHeightDelta =
            node.scrollHeight - previousNativeScrollHeight;
          lastNativeScrollTopRef.current = node.scrollTop;
          revokeMessageStackNativeScrollOwnershipOnConflict(node, scrollDelta);
          const nativeScrollOwnership =
            peekMessageStackNativeScrollOwnership(node);
          const isProgrammaticNavigation =
            pendingProgrammaticNavigationUntilRef.current >= performance.now();
          const isStableHeightNativeUserMovement =
            resolveStableHeightNativeUserMovement({
              currentScrollHeight: node.scrollHeight,
              previousScrollHeight: previousNativeScrollHeight,
              scrollDelta,
            });
          // A deferred mount or remeasurement can increase content height
          // between native scroll events. An upward scrollTop delta across
          // non-shrinking content is still reader movement; prepend/compaction
          // restores move with their height delta and shrink clamps are below.
          const isNativeUserMovement =
            isStableHeightNativeUserMovement ||
            (scrollDelta < 0 && scrollHeightDelta >= 0);
          if (
            nativeScrollAdvancesUserScrollGeneration({
              isExpectedPrependNativeReflow,
              isNativeUserMovement,
              isProgrammaticNavigation,
            })
          ) {
            // Scrollbar drags, touch inertia, and browser navigation can arrive
            // without an input prelude. Only the exact one-shot prepend reflow
            // token may preserve the generation across such a native frame.
            advanceUserScrollGeneration();
          }
          if (Math.abs(scrollDelta) >= 0.5) {
            pendingPrependedBottomGapRef.current = null;
          }
          const isPassiveTailFollowScroll = nativeScrollKeepsPassiveTailFollow({
            hadUserScrollInteraction,
            isDetachedFromBottom: isDetachedFromBottomRef.current,
            isNativeUserMovement,
            isProgrammaticNavigation,
            scrollDelta,
            scrollHeightDelta,
            tailFollowIntent,
          });
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
            if (
              scrollDelta < 0 &&
              isNativeUserMovement &&
              !isProgrammaticNavigation
            ) {
              // Scrollbar-thumb drags and touch inertia can move upward with no
              // wheel/key/touch prelude. Transfer authority immediately even
              // while the viewport remains inside the near-bottom band.
              shouldKeepBottomAfterLayoutRef.current = false;
              isDetachedFromBottomRef.current = true;
              clearPendingDeferredBottomRestore();
            }
            releaseConversationSearchPinForUserScroll();
            setHasUserScrollInteraction(true);
            lastUserScrollInputTimeRef.current = performance.now();
            captureLatestVisibleMessageAnchor(node);
            scheduleIdleMountedRangeCompaction(userScrollAdjustmentCooldownMs);
          }
          // Scrollbar-thumb drags and touch-inertia scrolls have no guaranteed
          // pre-scroll input event. Without a synchronous range commit, React
          // can defer this continuous-event update while the browser paints
          // the new scroll position over spacer-only DOM. Keep every active
          // native-scroll range change paint-safe in both directions; the
          // reconciler still retains only its bounded working band.
          const shouldFlushActiveNativeScroll =
            !isPassiveTailFollowScroll && !isScrollContainerNearBottom(node);
          reconcileMountedRangeForNativeScroll(
            node,
            scrollDelta,
            lastUserScrollKindRef.current,
            { flush: shouldFlushActiveNativeScroll },
          );
          if (
            scrollDelta > 0.5 &&
            messageStackNativeScrollOwnershipMovesTowardBottom(
              nativeScrollOwnership,
            ) &&
            !isProgrammaticNavigation &&
            isScrollContainerAtPhysicalBottom(node)
          ) {
            // The user just scrolled DOWN to the bottom. Re-arm the
            // bottom-follow flags so subsequent
            // layout changes can keep the view pinned, and clear the
            // user-interaction flag so the auto-scroll layout effect
            // does not bail when an incoming streamed delta grows
            // the layout past the near-bottom threshold for one
            // frame.
            //
            // A zero-delta event after a measurement shrink can also report
            // the detached viewport at the physical bottom. That geometry
            // transition is not reader intent and must not manufacture
            // bottom authority that a later growth can replay.
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

    const markUserScroll = (
      event?: WheelEvent | TouchEvent,
      explicitIntent?: MessageStackUserScrollIntentDetail,
    ) => {
      if (
        event &&
        (event.defaultPrevented ||
          (event.type === "wheel" &&
            isMessageStackWheelEventSuppressed(event)))
      ) {
        // The pane's capture-phase wheel arbiter rejected a residual gesture
        // that lost authority to newer keyboard navigation. Do not let that
        // stale tick advance virtualizer generations or restore direction.
        return;
      }
      let wheelDeltaY: number | null = null;
      let touchDeltaY: number | null = null;
      if (event?.type === "wheel" && "deltaY" in event) {
        const wheelEvent = event as WheelEvent;
        const wheelRouting = resolveMessageStackWheelRouting(wheelEvent, node);
        wheelDeltaY = wheelRouting.deltaY;
        if (
          wheelEvent.ctrlKey ||
          Math.abs(wheelDeltaY) < 0.5 ||
          wheelRouting.nestedScrollableConsumes
        ) {
          return;
        }
      } else if (
        typeof TouchEvent !== "undefined" &&
        event instanceof TouchEvent
      ) {
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
      const {
        inputCanMoveViewport,
        invalidatesPrependAuthority,
        isAttachedDownwardBoundaryInput,
      } = resolveVirtualizedInputMovementAuthority({
        bottomGapBeforeInput,
        explicitViewportCanMove: explicitIntent?.viewportCanMove,
        inputScrollDeltaY,
        isDetachedFromBottom: isDetachedFromBottomRef.current,
        scrollTop: node.scrollTop,
        shouldKeepBottom: shouldKeepBottomAfterLayoutRef.current,
      });
      if (invalidatesPrependAuthority) {
        pendingPrependNativeReflowRef.current = null;
        advanceUserScrollGeneration();
      }
      if (
        inputCanMoveViewport &&
        inputScrollDeltaY !== null &&
        Math.abs(inputScrollDeltaY) >= 0.5
      ) {
        claimMessageStackNativeScrollOwnership(
          node,
          {
            direction: inputScrollDeltaY < 0 ? "up" : "down",
            owner:
              typeof WheelEvent !== "undefined" && event instanceof WheelEvent
                ? "wheel"
                : "touch",
          },
          typeof WheelEvent !== "undefined" && event instanceof WheelEvent
            ? MESSAGE_STACK_WHEEL_OWNERSHIP_MS
            : MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS,
        );
      }
      const upwardInputDeltaPx =
        inputScrollDeltaY !== null && inputScrollDeltaY < 0
          ? Math.abs(inputScrollDeltaY)
          : null;
      const isLikelyBottomEscape =
        upwardInputDeltaPx !== null
          ? bottomGapBeforeInput <=
            SESSION_STICKY_BOTTOM_BAND_PX + upwardInputDeltaPx
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
                  visibleAnchorBeforeNativeScroll.viewportOffsetPx -
                  wheelDeltaY,
              }
            : visibleAnchorBeforeNativeScroll;
      }
      pendingProgrammaticBottomFollowUntilRef.current =
        Number.NEGATIVE_INFINITY;
      pendingProgrammaticNavigationUntilRef.current = Number.NEGATIVE_INFINITY;
      if (!isAttachedDownwardBoundaryInput) {
        pendingProgrammaticScrollTopRef.current = null;
      }
      pendingPrependedTopBoundaryRef.current = false;
      if (upwardInputDeltaPx === null || !isLikelyBottomEscape) {
        pendingPrependedBottomGapRef.current = null;
      }
      releaseConversationSearchPinForUserScroll();
      if (inputCanMoveViewport) {
        cancelBottomBoundaryRestore();
      }
      suspendDeferredRenderActivation(node);
      // User input invalidates the generic prepend/anchor/range layout timer.
      // The page-measurement bottom retry uses a dedicated timer and is cleared
      // below only when this gesture actually detaches the reader.
      clearPendingDeferredLayoutTimer();
      pendingDeferredLayoutAnchorRef.current = null;
      const isPageJumpKeyboardNavigation =
        explicitIntent?.scrollKind === "page_jump";
      const isExplicitUpwardScrollIntent =
        explicitIntent?.direction === "up" ||
        (wheelDeltaY !== null && wheelDeltaY < 0) ||
        (touchDeltaY !== null && touchDeltaY < 0);
      if (isExplicitUpwardScrollIntent) {
        clearPendingDeferredBottomRestore();
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
        // scroll lands outside the shared sticky-bottom band keeps the
        // bottom-pin armed long enough for a later layout tick to snap the
        // viewport back down once.
        shouldKeepBottomAfterLayoutRef.current = false;
        isDetachedFromBottomRef.current = true;
      }
      if (!isAttachedDownwardBoundaryInput) {
        lastUserScrollKindRef.current = isPageJumpKeyboardNavigation
          ? "page_jump"
          : "incremental";
        setHasUserScrollInteraction(true);
      }
      // A downward wheel already at the attached physical bottom cannot move
      // the viewport, so it must not detach or replace an earlier landing
      // lease. It still refreshes the measurement cooldown: content can grow
      // immediately after the input and bottom correction must wait until that
      // gesture window expires.
      lastUserScrollInputTimeRef.current = performance.now();
      scheduleIdleMountedRangeCompaction(userScrollAdjustmentCooldownMs);
      if (inputScrollDeltaY !== null && inputScrollDeltaY < 0) {
        prewarmMountedRangeForUpwardWheel(node, inputScrollDeltaY);
      }
    };
    const syncExternalUserScrollIntent = (event: Event) => {
      const detail =
        event instanceof CustomEvent
          ? (event.detail as MessageStackUserScrollIntentDetail | undefined)
          : undefined;
      if (!detail) {
        return;
      }
      if (!detail.viewportCanMove && !detail.detachFromBottomAtBoundary) {
        // An ordinary boundary-only intent belongs to history demand and must
        // not create virtualizer movement authority. The producer opts into the
        // exception only when an immovable upward gesture will hydrate older
        // history; in that case retained bottom authority could hide the prepend.
        return;
      }
      // Reuse the exact node-owned intent path. This clears programmatic
      // bottom-follow authority, advances restore generations, and records the
      // user cooldown before the body-targeted key produces native motion.
      markUserScroll(undefined, detail);
    };
    const syncProgrammaticScrollWrite = (event: Event) => {
      const explicitScrollKind =
        event instanceof CustomEvent
          ? ((event.detail as MessageStackScrollWriteDetail | undefined)
              ?.scrollKind ?? null)
          : null;
      const explicitScrollSource =
        event instanceof CustomEvent
          ? ((event.detail as MessageStackScrollWriteDetail | undefined)
              ?.scrollSource ?? "programmatic")
          : "programmatic";
      if (explicitScrollSource === "user") {
        // The pre-navigation anchor belongs to the old viewport. Clear it
        // explicitly; later spacer-only captures then preserve null, while a
        // measurable post-write slot installs the new anchor.
        latestVisibleMessageAnchorRef.current = null;
        pendingPrependNativeReflowRef.current = null;
        advanceUserScrollGeneration();
        // Pane-owned seek/page writes replace the old node-keydown prelude.
        // Cancel activation or boundary restore before its pending bottom write
        // can undo an explicit Home/End/PageUp/PageDown navigation.
        cancelBottomBoundaryRestore();
        // Direct pane-owned page/seek writes have no native wheel/touch
        // prelude. Transfer scroll ownership here as well so residual native
        // events from a canceled smooth bottom-follow animation cannot be
        // mistaken for continuation of that animation.
        pendingProgrammaticBottomFollowUntilRef.current =
          Number.NEGATIVE_INFINITY;
        pendingProgrammaticNavigationUntilRef.current =
          Number.NEGATIVE_INFINITY;
      }

      if (explicitScrollKind === "position_restore") {
        latestVisibleMessageAnchorRef.current = null;
        // A pane-owned detached restore is newer authority than any mounted-
        // range or anchor correction captured from the outgoing DOM. Those
        // delayed records already carry this generation; advance it before
        // reconciling the restored range so a literal old-bottom target cannot
        // land after the absolute saved position.
        advanceUserScrollGeneration();
        const previousScrollTop = lastNativeScrollTopRef.current;
        const scrollDelta = node.scrollTop - previousScrollTop;
        pendingProgrammaticBottomFollowUntilRef.current =
          Number.NEGATIVE_INFINITY;
        pendingProgrammaticScrollTopRef.current =
          Math.abs(scrollDelta) >= 0.5 ? node.scrollTop : null;
        lastNativeScrollTopRef.current = node.scrollTop;
        shouldKeepBottomAfterLayoutRef.current = false;
        isDetachedFromBottomRef.current = true;
        setHasUserScrollInteraction(false);
        pendingAggressiveIdleCompactionRef.current = true;
        pendingMountedPrependRestoreRef.current = null;
        skipNextMountedPrependRestoreRef.current = false;
        clearPendingDeferredLayoutTimer();
        clearPendingDeferredBottomRestore();
        clearPendingIdleCompactionTimer();
        // Handle-owned restores install their new deferred anchor after this
        // synchronous listener returns; this clear removes only stale anchor
        // authority from the outgoing scroll scope.
        pendingDeferredLayoutAnchorRef.current = null;
        // Mounted-range resolution consults the viewport snapshot during the
        // same render. Publish the restored DOM position first so it cannot
        // discard the requested range using the previous tab's scrollTop.
        syncViewportFromScrollNode(node);
        // A tab can restore to the same numeric scrollTop used by the outgoing
        // session while needing a completely different mounted page range.
        reconcileMountedRangeForNativeScroll(node, scrollDelta, "seek", {
          allowSeekFlush: false,
        });
        lastUserScrollKindRef.current = null;
        lastUserScrollInputTimeRef.current = Number.NEGATIVE_INFINITY;
        scheduleProgrammaticViewportSync(node);
        return;
      }

      if (
        explicitScrollKind === "bottom_pin" ||
        explicitScrollKind === "bottom_boundary"
      ) {
        latestVisibleMessageAnchorRef.current = null;
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
        clearPendingDeferredBottomRestore();
        clearPendingIdleCompactionTimer();
        pendingDeferredLayoutAnchorRef.current = null;
        syncViewportFromScrollNode(node);
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
        latestVisibleMessageAnchorRef.current = null;
        pendingPrependedTopBoundaryRef.current = false;
        pendingPrependedBottomGapRef.current = null;
        pendingProgrammaticBottomFollowUntilRef.current =
          performance.now() + MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS;
        enterBottomFollowMode();
        pendingAggressiveIdleCompactionRef.current = true;
        pendingMountedPrependRestoreRef.current = null;
        skipNextMountedPrependRestoreRef.current = false;
        clearPendingDeferredLayoutTimer();
        clearPendingDeferredBottomRestore();
        clearPendingIdleCompactionTimer();
        pendingDeferredLayoutAnchorRef.current = null;
        syncViewportFromScrollNode(node);
        // Bottom-follow only needs the tail pages to be resident. Keep an
        // existing wider band intact instead of narrowing it for one render
        // and letting viewport reconciliation expand it again immediately.
        applyMountedPageRange(buildBottomMountedRange(node.clientHeight), {
          preserveCoveringRange: true,
        });
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
      // The wide sticky band absorbs measurement jitter while layout-owned
      // follow is already attached. It must not reclaim authority from an
      // explicit wheel/page write: manual navigation stays detached until the
      // reachable physical bottom. Otherwise the pane and virtualizer disagree
      // for one commit and a page measurement can snap the viewport downward.
      const hasBottomAuthorityAfterWrite =
        explicitScrollSource === "user"
          ? isScrollContainerAtPhysicalBottom(node)
          : isNearBottomAfterWrite;
      if (pendingPrependedTopBoundaryRef.current || isNearBottomAfterWrite) {
        pendingPrependedBottomGapRef.current = null;
      }
      if (!isNearBottomAfterWrite) {
        suspendDeferredRenderActivation(node);
      }
      if (
        shouldKeepBottomAfterLayoutRef.current &&
        !hasBottomAuthorityAfterWrite
      ) {
        shouldKeepBottomAfterLayoutRef.current = false;
      }
      if (explicitScrollSource === "user" && !hasBottomAuthorityAfterWrite) {
        clearPendingDeferredBottomRestore();
        isDetachedFromBottomRef.current = true;
        setHasUserScrollInteraction(true);
      }
      if (hasBottomAuthorityAfterWrite) {
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
        if (explicitScrollKind === "seek" && hasBottomAuthorityAfterWrite) {
          shouldKeepBottomAfterLayoutRef.current = true;
          isDetachedFromBottomRef.current = false;
        }
        const resolvedScrollKind =
          explicitScrollKind ??
          (hasBottomAuthorityAfterWrite
            ? "seek"
            : classifyScrollKind(scrollDelta, node.clientHeight));
        const scrollWriteTime = performance.now();
        lastUserScrollKindRef.current = hasBottomAuthorityAfterWrite
          ? null
          : resolvedScrollKind;
        if (!hasBottomAuthorityAfterWrite) {
          if (explicitScrollSource === "user") {
            releaseConversationSearchPinForUserScroll();
          }
          lastUserScrollInputTimeRef.current = scrollWriteTime;
          scheduleIdleMountedRangeCompaction(userScrollAdjustmentCooldownMs);
        }
        const isActiveUpwardUserScrollWrite =
          explicitScrollSource === "user" &&
          scrollDelta < 0 &&
          !hasBottomAuthorityAfterWrite;
        const shouldRefreshUserScrollAnchor =
          explicitScrollSource === "user" && !hasBottomAuthorityAfterWrite;
        if (shouldRefreshUserScrollAnchor) {
          // The normalized intent captures an anchor before a pane-owned
          // Arrow/Page/wheel write. Replace it before range reconciliation:
          // that path may flush newly mounted pages whose layout effects
          // measure synchronously and must already see the post-write anchor.
          captureLatestVisibleMessageAnchor(node);
        }
        reconcileMountedRangeForNativeScroll(
          node,
          scrollDelta,
          resolvedScrollKind,
          {
            allowSeekFlush: explicitScrollSource === "user",
            flush: isActiveUpwardUserScrollWrite,
          },
        );
        if (shouldRefreshUserScrollAnchor) {
          // Refine once more against any DOM mounted by the synchronous range
          // commit. Later ResizeObserver delivery then preserves the reader's
          // deliberate movement instead of writing the old position back.
          captureLatestVisibleMessageAnchor(node);
        }
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
      pendingPrependNativeReflowRef.current = null;
      pendingProgrammaticBottomFollowUntilRef.current =
        Number.NEGATIVE_INFINITY;
      pendingProgrammaticNavigationUntilRef.current = Number.NEGATIVE_INFINITY;
      if (event.target === node) {
        claimMessageStackNativeScrollOwnership(
          node,
          { direction: null, owner: "pointer" },
          MESSAGE_STACK_POINTER_OWNERSHIP_MS,
        );
        advanceUserScrollGeneration();
        pendingProgrammaticScrollTopRef.current = null;
        shouldKeepBottomAfterLayoutRef.current = false;
        isDetachedFromBottomRef.current = true;
        setHasUserScrollInteraction(true);
        lastUserScrollInputTimeRef.current = performance.now();
      }
    };
    const recordTouchStart = (event: TouchEvent) => {
      pendingPrependNativeReflowRef.current = null;
      lastTouchClientYRef.current = event.touches[0]?.clientY ?? null;
      claimMessageStackNativeScrollOwnership(
        node,
        { direction: null, owner: "touch" },
        MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS,
      );
    };
    const recordTouchEnd = (event: TouchEvent) => {
      lastTouchClientYRef.current = event.touches[0]?.clientY ?? null;
      const currentOwnership = peekMessageStackNativeScrollOwnership(node);
      if (currentOwnership?.owner === "touch") {
        claimMessageStackNativeScrollOwnership(
          node,
          currentOwnership,
          MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS,
        );
      }
    };

    syncViewport();
    lastNativeScrollTopRef.current = node.scrollTop;
    lastNativeScrollHeightRef.current = node.scrollHeight;
    const onNativeScroll = () => {
      syncViewport({ isNativeScrollEvent: true });
    };
    node.addEventListener("scroll", onNativeScroll, { passive: true });
    node.addEventListener(
      MESSAGE_STACK_SCROLL_WRITE_EVENT,
      syncProgrammaticScrollWrite,
    );
    node.addEventListener(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      syncExternalUserScrollIntent,
    );
    node.addEventListener("wheel", markUserScroll, { passive: true });
    node.addEventListener("touchstart", recordTouchStart, { passive: true });
    node.addEventListener("touchmove", markUserScroll, { passive: true });
    node.addEventListener("touchend", recordTouchEnd, { passive: true });
    node.addEventListener("touchcancel", recordTouchEnd, { passive: true });
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
      node.removeEventListener(
        MESSAGE_STACK_SCROLL_WRITE_EVENT,
        syncProgrammaticScrollWrite,
      );
      node.removeEventListener(
        MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
        syncExternalUserScrollIntent,
      );
      node.removeEventListener("wheel", markUserScroll);
      node.removeEventListener("touchstart", recordTouchStart);
      node.removeEventListener("touchmove", markUserScroll);
      node.removeEventListener("touchend", recordTouchEnd);
      node.removeEventListener("touchcancel", recordTouchEnd);
      node.removeEventListener("mousedown", cancelBottomFollowOnMouseDown);
      resizeObserver?.disconnect();
    };
  }, [
    advanceUserScrollGeneration,
    applyMountedPageRange,
    buildBottomMountedRange,
    cancelPostActivationBottomRestore,
    captureLatestVisibleMessageAnchor,
    clearPendingDeferredBottomRestore,
    clearPendingDeferredLayoutTimer,
    clearPendingIdleCompactionTimer,
    getUserScrollGeneration,
    isActive,
    isMeasuringPostActivation,
    lastNativeScrollHeightRef,
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
