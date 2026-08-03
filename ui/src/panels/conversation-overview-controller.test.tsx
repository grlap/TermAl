import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { SessionOverviewResponse } from "../api";
import {
  conversationOverviewViewportFromResidentWindow,
  useConversationOverviewController,
} from "./conversation-overview-controller";
import type { VirtualizedConversationMessageListHandle } from "./VirtualizedConversationMessageList";

const { fetchSessionOverview, requestSessionHistoryAroundPage } = vi.hoisted(
  () => ({
    fetchSessionOverview: vi.fn(),
    requestSessionHistoryAroundPage: vi.fn(),
  }),
);

vi.mock("../api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api")>();
  return { ...actual, fetchSessionOverview };
});

vi.mock("../session-history-demand", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("../session-history-demand")>();
  return { ...actual, requestSessionHistoryAroundPage };
});

function overview(stamp = 1): SessionOverviewResponse {
  return {
    sessionId: "session-overview",
    messageCount: 100,
    sessionMutationStamp: stamp,
    buckets: [{ c: 100, k: "text", u: 10, m: false }],
    markers: [],
    latestPosition: 99,
  };
}

function controllerProps(
  overrides: Partial<
    Parameters<typeof useConversationOverviewController>[0]
  > = {},
) {
  return {
    isActive: true,
    messageCount: 100,
    messageStartIndex: 70,
    renderedMessageCount: 30,
    scrollContainerRef: { current: null },
    sessionId: "session-overview",
    sessionMutationStamp: 1,
    ...overrides,
  };
}

describe("useConversationOverviewController", () => {
  beforeEach(() => {
    fetchSessionOverview.mockReset();
    requestSessionHistoryAroundPage.mockReset();
    fetchSessionOverview.mockResolvedValue(overview());
    requestSessionHistoryAroundPage.mockResolvedValue(false);
  });

  it("fetches once on pane activation and refreshes on mutation metadata", async () => {
    const { result, rerender } = renderHook(
      (props) => useConversationOverviewController(props),
      { initialProps: controllerProps() },
    );

    await waitFor(() => expect(result.current.overview).toEqual(overview()));
    expect(fetchSessionOverview).toHaveBeenCalledTimes(1);
    expect(fetchSessionOverview).toHaveBeenCalledWith("session-overview");

    fetchSessionOverview.mockResolvedValue(overview(2));
    rerender(controllerProps({ sessionMutationStamp: 2 }));

    await waitFor(() =>
      expect(result.current.overview?.sessionMutationStamp).toBe(2),
    );
    expect(fetchSessionOverview).toHaveBeenCalledTimes(2);
  });

  it("coalesces a burst of overview freshness changes", async () => {
    vi.useFakeTimers();
    try {
      const { rerender } = renderHook(
        (props) => useConversationOverviewController(props),
        { initialProps: controllerProps() },
      );

      await act(async () => {
        await vi.runOnlyPendingTimersAsync();
      });
      expect(fetchSessionOverview).toHaveBeenCalledTimes(1);

      act(() => {
        rerender(controllerProps({ sessionMutationStamp: 2 }));
        rerender(controllerProps({ sessionMutationStamp: 3 }));
        rerender(controllerProps({ sessionMutationStamp: 4 }));
      });
      expect(fetchSessionOverview).toHaveBeenCalledTimes(1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(120);
      });
      expect(fetchSessionOverview).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("refreshes within the max wait during continuous streaming", async () => {
    vi.useFakeTimers();
    try {
      const { rerender } = renderHook(
        (props) => useConversationOverviewController(props),
        { initialProps: controllerProps() },
      );
      await act(async () => {
        await vi.runOnlyPendingTimersAsync();
      });
      expect(fetchSessionOverview).toHaveBeenCalledTimes(1);

      for (let stamp = 2; stamp <= 6; stamp += 1) {
        act(() => rerender(controllerProps({ sessionMutationStamp: stamp })));
        await act(async () => {
          await vi.advanceTimersByTimeAsync(100);
        });
      }

      expect(fetchSessionOverview).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("maps the resident scroll fraction into global positions", () => {
    expect(
      conversationOverviewViewportFromResidentWindow({
        messageCount: 1_000,
        messageStartIndex: 700,
        renderedMessageCount: 300,
        scrollNode: {
          clientHeight: 200,
          scrollHeight: 1_000,
          scrollTop: 400,
        },
      }),
    ).toEqual({
      startPosition: 820,
      endPosition: 880,
    });
  });

  it("keeps the rail frame anchored to the scroll viewport across wheel scrolls", async () => {
    const pane = document.createElement("section");
    pane.className = "workspace-pane";
    const scrollNode = document.createElement("section");
    pane.append(scrollNode);
    let scrollTop = 0;
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 600 },
      scrollHeight: { configurable: true, value: 1200 },
      scrollTop: {
        configurable: true,
        get: () => scrollTop,
        set: (nextValue: number) => {
          scrollTop = nextValue;
        },
      },
    });
    scrollNode.getBoundingClientRect = () =>
      ({
        bottom: 700,
        height: 600,
        left: 100,
        right: 900,
        top: 100,
        width: 800,
        x: 100,
        y: 100,
        toJSON: () => ({}),
      }) as DOMRect;
    pane.getBoundingClientRect = () =>
      ({
        bottom: 760,
        height: 740,
        left: 40,
        right: 1_000,
        top: 20,
        width: 960,
        x: 40,
        y: 20,
        toJSON: () => ({}),
      }) as DOMRect;

    const { result } = renderHook(() =>
      useConversationOverviewController(
        controllerProps({ scrollContainerRef: { current: scrollNode } }),
      ),
    );
    const expectedRight = 112;
    await waitFor(() => {
      expect(result.current.railHeightPx).toBe(576);
      expect(result.current.railPortalTarget).toBe(pane);
      expect(result.current.railRightPx).toBe(expectedRight);
      expect(result.current.railTopPx).toBe(92);
    });

    act(() => {
      scrollTop = 300;
      scrollNode.dispatchEvent(new Event("scroll"));
    });

    expect(result.current.railHeightPx).toBe(576);
    expect(result.current.railRightPx).toBe(expectedRight);
    expect(result.current.railTopPx).toBe(92);
  });

  it("clears pane-local rail geometry as soon as the transcript deactivates", async () => {
    const pane = document.createElement("section");
    pane.className = "workspace-pane";
    const scrollNode = document.createElement("section");
    pane.append(scrollNode);
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      value: 600,
    });

    const { result, rerender } = renderHook(
      (props) => useConversationOverviewController(props),
      {
        initialProps: controllerProps({
          scrollContainerRef: { current: scrollNode },
        }),
      },
    );

    await waitFor(() => expect(result.current.railPortalTarget).toBe(pane));
    rerender(
      controllerProps({
        isActive: false,
        scrollContainerRef: { current: scrollNode },
      }),
    );
    expect(result.current.railPortalTarget).toBeNull();
    expect(result.current.shouldRenderRail).toBe(false);
  });

  it("ignores an overview response that resolves after deactivation", async () => {
    let resolveOverview: ((value: SessionOverviewResponse) => void) | null =
      null;
    fetchSessionOverview.mockImplementationOnce(
      () =>
        new Promise<SessionOverviewResponse>((resolve) => {
          resolveOverview = resolve;
        }),
    );
    const { result, rerender } = renderHook(
      (props) => useConversationOverviewController(props),
      { initialProps: controllerProps() },
    );

    await waitFor(() => expect(fetchSessionOverview).toHaveBeenCalledTimes(1));
    rerender(controllerProps({ isActive: false }));
    act(() => resolveOverview?.(overview()));
    await act(async () => Promise.resolve());

    expect(result.current.overview).toBeNull();
    expect(result.current.shouldRenderRail).toBe(false);
  });

  it("jumps resident positions without loading history", async () => {
    const { result } = renderHook(() =>
      useConversationOverviewController(controllerProps()),
    );
    await waitFor(() => expect(result.current.overview).toEqual(overview()));
    const jumpToMessageIndex = vi.fn(() => true);
    result.current.virtualizerHandleRef.current = {
      jumpToMessageIndex,
    } as unknown as VirtualizedConversationMessageListHandle;

    act(() => result.current.navigate(80));

    expect(jumpToMessageIndex).toHaveBeenCalledWith(10, {
      align: "center",
      flush: true,
    });
    expect(requestSessionHistoryAroundPage).not.toHaveBeenCalled();
  });

  it("requests one centered page for off-window positions", async () => {
    const { result } = renderHook(() =>
      useConversationOverviewController(controllerProps()),
    );
    await waitFor(() => expect(result.current.overview).toEqual(overview()));
    result.current.virtualizerHandleRef.current = {
      jumpToMessageIndex: vi.fn(() => false),
    } as unknown as VirtualizedConversationMessageListHandle;

    act(() => result.current.navigate(20));

    expect(requestSessionHistoryAroundPage).toHaveBeenCalledWith(
      "session-overview",
      20,
    );
  });
});
