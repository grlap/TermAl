// Owns focused tests for generation-based arbitration between user navigation
// and delayed mounted-range restores.
// Does not own React event wiring, page measurement, or rendered scroll bands.

import { renderHook } from "@testing-library/react";
import type { MutableRefObject } from "react";
import { describe, expect, it, vi } from "vitest";

import {
  mountedPrependRestoreIsCurrent,
  type MountedPrependRestore,
} from "./virtualized-conversation-mounted-range";
import type { PendingVisibleMessageAnchor } from "./virtualized-conversation-measurement";
import { useVirtualizedConversationPrependEffects } from "./virtualized-conversation-prepend";
import {
  nativeScrollAdvancesUserScrollGeneration,
  nativeScrollKeepsPassiveTailFollow,
  pendingPrependNativeReflowMatches,
  resolveBottomLandingClamp,
  resolveStableHeightNativeUserMovement,
  resolveVirtualizedInputMovementAuthority,
} from "./virtualized-conversation-scroll-events";

function restoreAtGeneration(
  userScrollGeneration: number,
): MountedPrependRestore {
  return {
    anchor: null,
    scrollHeight: 2_000,
    scrollTop: 800,
    writeIntent: "mounted-range",
    userScrollGeneration,
  };
}

describe("mounted prepend restore generation", () => {
  it("keeps a prepend or idle-compaction restore while user position ownership is unchanged", () => {
    expect(mountedPrependRestoreIsCurrent(restoreAtGeneration(4), 4)).toBe(
      true,
    );
  });

  it("rejects an idle-compaction restore when a later wheel takes ownership", () => {
    expect(mountedPrependRestoreIsCurrent(restoreAtGeneration(4), 5)).toBe(
      false,
    );
  });

  it("rejects a prepend restore captured before a later PageUp navigation", () => {
    expect(mountedPrependRestoreIsCurrent(restoreAtGeneration(8), 9)).toBe(
      false,
    );
  });

  it("rejects a pending visible anchor after newer user navigation", () => {
    let scrollTop = 860;
    const scrollWrites: number[] = [];
    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      value: 100,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
        scrollWrites.push(nextValue);
      },
    });
    scrollNode.getBoundingClientRect = () =>
      ({ top: 0, bottom: 100 } as DOMRect);

    const renderedList = document.createElement("div");
    const anchorSlot = document.createElement("div");
    anchorSlot.className = "virtualized-message-slot";
    anchorSlot.dataset.messageId = "message-anchor";
    anchorSlot.getBoundingClientRect = () =>
      ({ top: 40, bottom: 120 } as DOMRect);
    renderedList.append(anchorSlot);

    const pendingPrependedMessageAnchorRef: MutableRefObject<
      PendingVisibleMessageAnchor | null
    > = {
      current: {
        messageId: "message-anchor",
        remainingAttempts: 3,
        userScrollGeneration: 7,
        viewportOffsetPx: 0,
      },
    };
    const hookArgs = {
      applyMountedPageRange: vi.fn(),
      buildWorkingMountedRangeForScrollTop: vi.fn(() => ({
        endIndex: 0,
        startIndex: 0,
      })),
      clearPendingDeferredLayoutTimer: vi.fn(),
      estimateMessageHeight: vi.fn(() => 80),
      getUserScrollGeneration: () => 8,
      hasUserScrollInteractionRef: { current: true },
      isActive: true,
      isDetachedFromBottomRef: { current: true },
      lastNativeScrollTopRef: { current: scrollTop },
      latestVisibleMessageAnchorRef: { current: null },
      layoutVersion: 1,
      messageLocationById: new Map(),
      messages: [],
      mountedPageRange: { endIndex: 0, startIndex: 0 },
      mountedPageRangeRef: { current: { endIndex: 0, startIndex: 0 } },
      pageLayout: { tops: [], totalHeight: 0 },
      pages: [],
      pendingMountedPrependRestoreRef: { current: null },
      pendingPrependedBottomGapRef: { current: null },
      pendingPrependedMessageAnchorRef,
      pendingPrependedTopBoundaryRef: { current: false },
      previousMessageWindowRef: {
        current: { ids: [], sessionId: "session-a" },
      },
      renderedListRef: { current: renderedList },
      scrollContainerRef: { current: scrollNode },
      sessionId: "session-a",
      shouldKeepBottomAfterLayoutRef: { current: false },
      skipNextMountedPrependRestoreRef: { current: false },
      viewportHeight: 100,
      writeScrollTopAndSyncViewport: (
        node: HTMLElement,
        nextScrollTop: number,
      ) => {
        node.scrollTop = nextScrollTop;
      },
    };

    const { unmount } = renderHook(() =>
      useVirtualizedConversationPrependEffects(hookArgs),
    );

    expect(pendingPrependedMessageAnchorRef.current).toBeNull();
    expect(scrollWrites).toEqual([]);
    expect(scrollTop).toBe(860);
    unmount();
  });

  it("does not treat a height-changing prepend reflow as user navigation", () => {
    expect(
      resolveStableHeightNativeUserMovement({
        currentScrollHeight: 12_000,
        previousScrollHeight: 2_000,
        scrollDelta: 600,
      }),
    ).toBe(false);
  });

  it("recognizes native thumb or inertia movement when layout height is stable", () => {
    expect(
      resolveStableHeightNativeUserMovement({
        currentScrollHeight: 12_000,
        previousScrollHeight: 12_000,
        scrollDelta: -600,
      }),
    ).toBe(true);
  });

  it("advances the generation for prelude-less native reader movement", () => {
    expect(
      nativeScrollAdvancesUserScrollGeneration({
        isExpectedPrependNativeReflow: false,
        isNativeUserMovement: true,
        isProgrammaticNavigation: false,
      }),
    ).toBe(true);
  });

  it("preserves the generation for the exact one-shot prepend reflow", () => {
    const node = document.createElement("div");
    Object.defineProperties(node, {
      scrollHeight: { configurable: true, value: 1_240 },
      scrollTop: { configurable: true, value: 440 },
    });

    const isExpectedPrependNativeReflow =
      pendingPrependNativeReflowMatches({
        currentUserScrollGeneration: 7,
        node,
        token: {
          expectedScrollHeight: 1_240,
          expectedScrollTop: 440,
          userScrollGeneration: 7,
        },
      });

    expect(isExpectedPrependNativeReflow).toBe(true);
    expect(
      nativeScrollAdvancesUserScrollGeneration({
        isExpectedPrependNativeReflow,
        isNativeUserMovement: true,
        isProgrammaticNavigation: false,
      }),
    ).toBe(false);
  });

  it("does not let a prepend token mask newer or geometrically different movement", () => {
    const node = document.createElement("div");
    Object.defineProperties(node, {
      scrollHeight: { configurable: true, value: 1_260 },
      scrollTop: { configurable: true, value: 420 },
    });
    const token = {
      expectedScrollHeight: 1_240,
      expectedScrollTop: 440,
      userScrollGeneration: 7,
    };

    expect(
      pendingPrependNativeReflowMatches({
        currentUserScrollGeneration: 8,
        node,
        token,
      }),
    ).toBe(false);
    expect(
      pendingPrependNativeReflowMatches({
        currentUserScrollGeneration: 7,
        node,
        token,
      }),
    ).toBe(false);
  });

  it("preserves prepend authority for an immovable downward boundary input", () => {
    expect(
      resolveVirtualizedInputMovementAuthority({
        bottomGapBeforeInput: 0,
        inputScrollDeltaY: 40,
        isDetachedFromBottom: false,
        scrollTop: 900,
        shouldKeepBottom: true,
      }),
    ).toEqual({
      inputCanMoveViewport: false,
      invalidatesPrependAuthority: false,
      isAttachedDownwardBoundaryInput: true,
    });
  });

  it("invalidates prepend authority when explicit keyboard intent can move", () => {
    expect(
      resolveVirtualizedInputMovementAuthority({
        bottomGapBeforeInput: 0,
        explicitViewportCanMove: true,
        inputScrollDeltaY: null,
        isDetachedFromBottom: false,
        scrollTop: 900,
        shouldKeepBottom: true,
      }),
    ).toEqual({
      inputCanMoveViewport: true,
      invalidatesPrependAuthority: true,
      isAttachedDownwardBoundaryInput: false,
    });
  });

  it("classifies every downward frame landing at the physical bottom as a clamp", () => {
    // Viewport growth clamp stays a clamp.
    expect(
      resolveBottomLandingClamp({
        isAtPhysicalBottom: true,
        isViewportGrowthClamp: true,
        scrollDelta: -40,
      }),
    ).toBe(true);
    // Sub-pixel jitter landing at the bottom stays a clamp.
    expect(
      resolveBottomLandingClamp({
        isAtPhysicalBottom: true,
        isViewportGrowthClamp: false,
        scrollDelta: -1,
      }),
    ).toBe(true);
    // A turn-end shrink (live-turn card unmounting) clamps scrollTop down and
    // lands the attached reader at the physical bottom of the shorter
    // content. That frame is a browser clamp, never reader movement — even
    // when a real user interaction happened recently.
    expect(
      resolveBottomLandingClamp({
        isAtPhysicalBottom: true,
        isViewportGrowthClamp: false,
        scrollDelta: -120,
      }),
    ).toBe(true);
    // An upward frame that ends above the bottom is reader movement.
    expect(
      resolveBottomLandingClamp({
        isAtPhysicalBottom: false,
        isViewportGrowthClamp: false,
        scrollDelta: -40,
      }),
    ).toBe(false);
  });

  it("keeps tail-follow through a bottom-landing clamp despite recent user interaction", () => {
    expect(
      nativeScrollKeepsPassiveTailFollow({
        hadUserScrollInteraction: true,
        isBottomLandingClamp: true,
        isDetachedFromBottom: false,
        isNativeUserMovement: false,
        isProgrammaticNavigation: false,
        scrollDelta: -120,
        scrollHeightDelta: -180,
        tailFollowIntent: true,
      }),
    ).toBe(true);
  });

  it("keeps tail-follow authority when content shrink clamps scrollTop upward", () => {
    const isNativeUserMovement = resolveStableHeightNativeUserMovement({
      currentScrollHeight: 11_998,
      previousScrollHeight: 12_000,
      scrollDelta: -2,
    });

    expect(isNativeUserMovement).toBe(false);
    expect(
      nativeScrollKeepsPassiveTailFollow({
        hadUserScrollInteraction: false,
        isDetachedFromBottom: false,
        isNativeUserMovement,
        isProgrammaticNavigation: false,
        isBottomLandingClamp: false,
        scrollDelta: -2,
        scrollHeightDelta: -2,
        tailFollowIntent: true,
      }),
    ).toBe(true);
  });

  it("transfers tail-follow authority for stable-height upward native movement", () => {
    const isNativeUserMovement = resolveStableHeightNativeUserMovement({
      currentScrollHeight: 12_000,
      previousScrollHeight: 12_000,
      scrollDelta: -2,
    });

    expect(isNativeUserMovement).toBe(true);
    expect(
      nativeScrollKeepsPassiveTailFollow({
        hadUserScrollInteraction: false,
        isDetachedFromBottom: false,
        isNativeUserMovement,
        isProgrammaticNavigation: false,
        isBottomLandingClamp: false,
        scrollDelta: -2,
        scrollHeightDelta: 0,
        tailFollowIntent: true,
      }),
    ).toBe(false);
  });

  it("transfers tail-follow authority when an upward frame follows content growth", () => {
    const isNativeUserMovement = resolveStableHeightNativeUserMovement({
      currentScrollHeight: 12_040,
      previousScrollHeight: 12_000,
      scrollDelta: -40,
    });

    expect(isNativeUserMovement).toBe(false);
    expect(
      nativeScrollKeepsPassiveTailFollow({
        hadUserScrollInteraction: false,
        isDetachedFromBottom: false,
        isNativeUserMovement,
        isProgrammaticNavigation: false,
        isBottomLandingClamp: false,
        scrollDelta: -40,
        scrollHeightDelta: 40,
        tailFollowIntent: true,
      }),
    ).toBe(false);
  });

  it("keeps an attached search jump passive during smooth upward native frames", () => {
    expect(
      nativeScrollKeepsPassiveTailFollow({
        hadUserScrollInteraction: false,
        isDetachedFromBottom: false,
        isNativeUserMovement: true,
        isProgrammaticNavigation: true,
        isBottomLandingClamp: false,
        scrollDelta: -600,
        scrollHeightDelta: 0,
        tailFollowIntent: true,
      }),
    ).toBe(true);
  });

  it("keeps a viewport-growth bottom clamp passive after prior user interaction", () => {
    expect(
      nativeScrollKeepsPassiveTailFollow({
        hadUserScrollInteraction: true,
        isDetachedFromBottom: false,
        isNativeUserMovement: false,
        isProgrammaticNavigation: false,
        isBottomLandingClamp: true,
        scrollDelta: -40,
        scrollHeightDelta: 0,
        tailFollowIntent: true,
      }),
    ).toBe(true);
  });

  it("does not let a viewport-growth clamp override detached reader authority", () => {
    expect(
      nativeScrollKeepsPassiveTailFollow({
        hadUserScrollInteraction: true,
        isDetachedFromBottom: true,
        isNativeUserMovement: false,
        isProgrammaticNavigation: false,
        isBottomLandingClamp: true,
        scrollDelta: -40,
        scrollHeightDelta: 0,
        tailFollowIntent: true,
      }),
    ).toBe(false);
  });

  it("does not manufacture tail-follow intent from a viewport-growth clamp", () => {
    expect(
      nativeScrollKeepsPassiveTailFollow({
        hadUserScrollInteraction: false,
        isDetachedFromBottom: false,
        isNativeUserMovement: false,
        isProgrammaticNavigation: false,
        isBottomLandingClamp: true,
        scrollDelta: -40,
        scrollHeightDelta: 0,
        tailFollowIntent: false,
      }),
    ).toBe(false);
  });
});
