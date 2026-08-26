// Owns commit-order regressions for virtualized page-height measurements.
// Does not own page rendering, ResizeObserver wiring, or scroll input policy.
import { act, render } from "@testing-library/react";
import { useLayoutEffect, useRef, type MutableRefObject } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useVirtualizedConversationPageHeightChange } from "./virtualized-conversation-page-heights";

afterEach(() => {
  vi.restoreAllMocks();
});

function MeasurementCaller({
  isActive,
  onHeightChange,
  observations,
  shouldKeepBottomAfterLayoutRef,
}: {
  isActive: boolean;
  onHeightChange: (
    pageKey: string,
    pageIndex: number,
    nextHeight: number,
  ) => void;
  observations: boolean[];
  shouldKeepBottomAfterLayoutRef: MutableRefObject<boolean>;
}) {
  useLayoutEffect(() => {
    onHeightChange("page-0", 0, isActive ? 120 : 100);
    observations.push(shouldKeepBottomAfterLayoutRef.current);
  }, [isActive, observations, onHeightChange, shouldKeepBottomAfterLayoutRef]);
  return null;
}

function Harness({
  isActive,
  observations,
  scrollNode,
}: {
  isActive: boolean;
  observations: boolean[];
  scrollNode: HTMLElement;
}) {
  const shouldKeepBottomAfterLayoutRef = useRef(false);
  const handlePageHeightChange = useVirtualizedConversationPageHeightChange({
    bumpLayoutVersion: vi.fn(),
    clearPendingDeferredLayoutTimer: vi.fn(),
    hasUserScrollInteractionRef: useRef(true),
    isActive,
    isDetachedFromBottomRef: useRef(false),
    isMessagePrependCommitRef: useRef(false),
    lastNativeScrollTopRef: useRef(500),
    lastUserScrollInputTimeRef: useRef(0),
    latestVisibleMessageAnchorRef: useRef(null),
    layoutPageHeightsRef: useRef({ "page-0": 100 }),
    measuredPageIdentityRef: useRef({}),
    pageHeightsRef: useRef({}),
    currentPageIdentityRef: useRef({
      "page-0": { hasTrailingGap: false, messages: [] },
    }),
    renderedListRef: useRef(null),
    scheduleDeferredLayoutVersion: vi.fn(),
    scrollContainerRef: { current: scrollNode },
    shouldKeepBottomAfterLayoutRef,
    userScrollAdjustmentCooldownMs: 120,
    visiblePageRangeRef: useRef({ startIndex: 0, endIndex: 1 }),
    writeScrollTopAndSyncViewport: vi.fn(),
  });

  return (
    <MeasurementCaller
      isActive={isActive}
      observations={observations}
      onHeightChange={handlePageHeightChange}
      shouldKeepBottomAfterLayoutRef={shouldKeepBottomAfterLayoutRef}
    />
  );
}

function RecentBottomReentryHarness({
  bumpLayoutVersion,
  onReady,
  scheduleDeferredLayoutVersion,
  scrollNode,
  writeScrollTopAndSyncViewport,
}: {
  bumpLayoutVersion: () => void;
  onReady: (
    onHeightChange: (
      pageKey: string,
      pageIndex: number,
      nextHeight: number,
      pageNode?: HTMLElement | null,
      flushLayout?: boolean,
    ) => void,
  ) => void;
  scheduleDeferredLayoutVersion: (delayMs: number) => void;
  scrollNode: HTMLElement;
  writeScrollTopAndSyncViewport: (
    node: HTMLElement,
    nextScrollTop: number,
  ) => void;
}) {
  const handlePageHeightChange = useVirtualizedConversationPageHeightChange({
    bumpLayoutVersion,
    clearPendingDeferredLayoutTimer: vi.fn(),
    hasUserScrollInteractionRef: useRef(false),
    isActive: true,
    isDetachedFromBottomRef: useRef(false),
    isMessagePrependCommitRef: useRef(false),
    lastNativeScrollTopRef: useRef(400),
    lastUserScrollInputTimeRef: useRef(900),
    latestVisibleMessageAnchorRef: useRef(null),
    layoutPageHeightsRef: useRef({ "page-0": 100 }),
    measuredPageIdentityRef: useRef({}),
    pageHeightsRef: useRef({}),
    currentPageIdentityRef: useRef({
      "page-0": { hasTrailingGap: false, messages: [] },
    }),
    renderedListRef: useRef(null),
    scheduleDeferredLayoutVersion,
    scrollContainerRef: { current: scrollNode },
    shouldKeepBottomAfterLayoutRef: useRef(true),
    userScrollAdjustmentCooldownMs: 200,
    visiblePageRangeRef: useRef({ startIndex: 0, endIndex: 1 }),
    writeScrollTopAndSyncViewport,
  });

  useLayoutEffect(() => onReady(handlePageHeightChange), [handlePageHeightChange, onReady]);
  return null;
}

describe("useVirtualizedConversationPageHeightChange", () => {
  it("uses the committed active state during descendant layout measurement", () => {
    const observations: boolean[] = [];
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 500 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, value: 500, writable: true },
    });
    const { rerender } = render(
      <Harness
        isActive={false}
        observations={observations}
        scrollNode={scrollNode}
      />,
    );

    rerender(
      <Harness
        isActive
        observations={observations}
        scrollNode={scrollNode}
      />,
    );

    expect(observations).toEqual([false, true]);
  });

  it("defers a recent physical-bottom reentry until the scroll cooldown expires", () => {
    vi.spyOn(performance, "now").mockReturnValue(1_000);
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 100 },
      scrollHeight: { configurable: true, value: 520 },
      scrollTop: { configurable: true, value: 400, writable: true },
    });
    const writeScrollTopAndSyncViewport = vi.fn();
    const scheduleDeferredLayoutVersion = vi.fn();
    const bumpLayoutVersion = vi.fn();
    let handlePageHeightChange:
      | ((
          pageKey: string,
          pageIndex: number,
          nextHeight: number,
          pageNode?: HTMLElement | null,
          flushLayout?: boolean,
        ) => void)
      | null = null;

    render(
      <RecentBottomReentryHarness
        bumpLayoutVersion={bumpLayoutVersion}
        onReady={(callback) => {
          handlePageHeightChange = callback;
        }}
        scheduleDeferredLayoutVersion={scheduleDeferredLayoutVersion}
        scrollNode={scrollNode}
        writeScrollTopAndSyncViewport={writeScrollTopAndSyncViewport}
      />,
    );
    expect(handlePageHeightChange).not.toBeNull();

    act(() => {
      handlePageHeightChange?.("page-0", 0, 120, null, true);
    });

    expect(writeScrollTopAndSyncViewport).not.toHaveBeenCalled();
    expect(bumpLayoutVersion).toHaveBeenCalledTimes(1);
    expect(scheduleDeferredLayoutVersion).toHaveBeenCalledTimes(1);
    expect(scheduleDeferredLayoutVersion).toHaveBeenCalledWith(100);
  });
});
