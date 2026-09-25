// Owns commit-order regressions for virtualized page-height measurements.
// Does not own page rendering, ResizeObserver wiring, or scroll input policy.
import { act, render } from "@testing-library/react";
import { useLayoutEffect, useRef, type MutableRefObject } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  shouldFlushVirtualizedPageHeightLayout,
  useVirtualizedConversationPageHeightChange,
} from "./virtualized-conversation-page-heights";
import type { PageMeasurementIdentity } from "./virtualized-conversation-measurement";

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
  clearPendingDeferredBottomRestore = vi.fn(),
  isActive,
  observations,
  scrollNode,
}: {
  clearPendingDeferredBottomRestore?: () => void;
  isActive: boolean;
  observations: boolean[];
  scrollNode: HTMLElement;
}) {
  const shouldKeepBottomAfterLayoutRef = useRef(false);
  const handlePageHeightChange = useVirtualizedConversationPageHeightChange({
    bumpLayoutVersion: vi.fn(),
    clearPendingDeferredBottomRestore,
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
    scheduleDeferredBottomRestoreLayoutVersion: vi.fn(),
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
  scheduleDeferredBottomRestoreLayoutVersion,
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
    measurements: Record<string, PageMeasurementIdentity>,
  ) => void;
  scheduleDeferredBottomRestoreLayoutVersion: (delayMs: number) => void;
  scrollNode: HTMLElement;
  writeScrollTopAndSyncViewport: (
    node: HTMLElement,
    nextScrollTop: number,
  ) => void;
}) {
  const measurements = useRef<Record<string, PageMeasurementIdentity>>({});
  const handlePageHeightChange = useVirtualizedConversationPageHeightChange({
    bumpLayoutVersion,
    clearPendingDeferredBottomRestore: vi.fn(),
    clearPendingDeferredLayoutTimer: vi.fn(),
    hasUserScrollInteractionRef: useRef(false),
    isActive: true,
    isDetachedFromBottomRef: useRef(false),
    isMessagePrependCommitRef: useRef(false),
    lastNativeScrollTopRef: useRef(400),
    lastUserScrollInputTimeRef: useRef(900),
    latestVisibleMessageAnchorRef: useRef(null),
    layoutPageHeightsRef: useRef({ "page-0": 100 }),
    measuredPageIdentityRef: measurements,
    pageHeightsRef: useRef({}),
    currentPageIdentityRef: useRef({
      "page-0": { hasTrailingGap: false, messages: [] },
    }),
    renderedListRef: useRef(null),
    scheduleDeferredBottomRestoreLayoutVersion,
    scheduleDeferredLayoutVersion: vi.fn(),
    scrollContainerRef: { current: scrollNode },
    shouldKeepBottomAfterLayoutRef: useRef(true),
    userScrollAdjustmentCooldownMs: 200,
    visiblePageRangeRef: useRef({ startIndex: 0, endIndex: 1 }),
    writeScrollTopAndSyncViewport,
  });

  useLayoutEffect(() => onReady(handlePageHeightChange, measurements.current), [handlePageHeightChange, onReady]);
  return null;
}

describe("useVirtualizedConversationPageHeightChange", () => {
  it("refreshes full-content provenance even when placeholder activation does not change height", () => {
    const node = document.createElement("section");
    const band = document.createElement("div");
    band.innerHTML = '<div class="virtualized-message-slot" data-message-id="full"></div>' +
      '<div class="virtualized-message-slot" data-message-id="preview"><div data-deferred-content-pending="true"></div></div>';
    render(<RecentBottomReentryHarness
      bumpLayoutVersion={vi.fn()}
      scheduleDeferredBottomRestoreLayoutVersion={vi.fn()}
      scrollNode={node}
      writeScrollTopAndSyncViewport={vi.fn()}
      onReady={(measure, identities) => {
        measure("page-0", 0, 100, band);
        expect(identities["page-0"]!.fullyRenderedMessageIds).toEqual(["full"]);
        band.querySelector("[data-deferred-content-pending]")!.removeAttribute("data-deferred-content-pending");
        measure("page-0", 0, 100, band);
        expect(identities["page-0"]!.fullyRenderedMessageIds).toEqual(["full", "preview"]);
      }}
    />);
  });

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

  it("clears pending bottom restoration when the pane has no bottom authority", () => {
    const clearPendingDeferredBottomRestore = vi.fn();
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 500 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, value: 200, writable: true },
    });

    render(
      <Harness
        clearPendingDeferredBottomRestore={clearPendingDeferredBottomRestore}
        isActive={false}
        observations={[]}
        scrollNode={scrollNode}
      />,
    );

    expect(clearPendingDeferredBottomRestore).toHaveBeenCalled();
  });

  it("flushes only when bottom restoration can run outside the user cooldown", () => {
    expect(
      shouldFlushVirtualizedPageHeightLayout({
        flushLayout: true,
        hasUserScrollInteraction: false,
        isInUserScrollCooldown: false,
        shouldKeepBottom: true,
      }),
    ).toBe(true);
    expect(
      shouldFlushVirtualizedPageHeightLayout({
        flushLayout: true,
        hasUserScrollInteraction: false,
        isInUserScrollCooldown: true,
        shouldKeepBottom: true,
      }),
    ).toBe(false);
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
    const scheduleDeferredBottomRestoreLayoutVersion = vi.fn();
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
        scheduleDeferredBottomRestoreLayoutVersion={
          scheduleDeferredBottomRestoreLayoutVersion
        }
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
    expect(scheduleDeferredBottomRestoreLayoutVersion).toHaveBeenCalledTimes(1);
    expect(scheduleDeferredBottomRestoreLayoutVersion).toHaveBeenCalledWith(100);
  });
});
