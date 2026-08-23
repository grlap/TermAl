import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { SessionOverviewResponse } from "../api";
import { MESSAGE_STACK_SCROLL_WRITE_EVENT } from "../message-stack-scroll-sync";
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

function overview(
  stamp = 1,
  sessionId = "session-overview",
): SessionOverviewResponse {
  return {
    sessionId,
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
    scrollStateKey: "pane-overview:session:session-overview",
    sessionId: "session-overview",
    sessionMutationStamp: 1,
    tailFollowIntent: true,
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

  it("refreshes on mutation metadata without replacing equal overview content", async () => {
    const { result, rerender } = renderHook(
      (props) => useConversationOverviewController(props),
      { initialProps: controllerProps() },
    );

    await waitFor(() => expect(result.current.overview).toEqual(overview()));
    expect(fetchSessionOverview).toHaveBeenCalledTimes(1);
    expect(fetchSessionOverview).toHaveBeenCalledWith("session-overview");

    const firstOverview = result.current.overview;
    fetchSessionOverview.mockResolvedValue(overview(2));
    rerender(controllerProps({ sessionMutationStamp: 2 }));

    await waitFor(() => expect(fetchSessionOverview).toHaveBeenCalledTimes(2));
    await act(async () => Promise.resolve());
    expect(result.current.overview).toBe(firstOverview);
    expect(result.current.overview?.sessionMutationStamp).toBe(1);
  });

  it("adopts a refreshed overview when its projected content changes", async () => {
    const { result, rerender } = renderHook(
      (props) => useConversationOverviewController(props),
      { initialProps: controllerProps() },
    );
    await waitFor(() => expect(result.current.overview).toEqual(overview()));
    const firstOverview = result.current.overview;
    const changedOverview: SessionOverviewResponse = {
      ...overview(2),
      buckets: [{ c: 100, k: "error", u: 10, m: false }],
    };
    fetchSessionOverview.mockResolvedValue(changedOverview);

    rerender(controllerProps({ sessionMutationStamp: 2 }));

    await waitFor(() => expect(result.current.overview).toBe(changedOverview));
    expect(result.current.overview).not.toBe(firstOverview);
  });

  it("renders a pending rail while the initial overview is loading", async () => {
    fetchSessionOverview.mockImplementationOnce(
      () => new Promise<SessionOverviewResponse>(() => {}),
    );

    const { result } = renderHook(() =>
      useConversationOverviewController(controllerProps()),
    );

    await waitFor(() => expect(fetchSessionOverview).toHaveBeenCalledTimes(1));
    expect(result.current.overview).toBeNull();
    expect(result.current.isRailReady).toBe(false);
    expect(result.current.shouldRender).toBe(false);
    expect(result.current.shouldRenderRail).toBe(true);
  });

  it("keeps the pending rail after an overview request is rejected", async () => {
    fetchSessionOverview.mockRejectedValueOnce(new Error("overview unavailable"));

    const { result } = renderHook(() =>
      useConversationOverviewController(controllerProps()),
    );

    await waitFor(() => expect(fetchSessionOverview).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(result.current.overview).toBeNull());
    expect(result.current.isRailReady).toBe(false);
    expect(result.current.shouldRender).toBe(false);
    expect(result.current.shouldRenderRail).toBe(true);
  });

  it("enables the overview layout atomically when the response becomes ready", async () => {
    let resolveOverview: ((value: SessionOverviewResponse) => void) | null = null;
    fetchSessionOverview.mockImplementationOnce(
      () =>
        new Promise<SessionOverviewResponse>((resolve) => {
          resolveOverview = resolve;
        }),
    );
    const { result } = renderHook(() =>
      useConversationOverviewController(controllerProps()),
    );

    await waitFor(() => expect(fetchSessionOverview).toHaveBeenCalledTimes(1));
    expect(result.current.shouldRender).toBe(false);
    act(() => resolveOverview?.(overview()));

    await waitFor(() => expect(result.current.shouldRender).toBe(true));
    expect(result.current.overview).toEqual(overview());
    expect(result.current.isRailReady).toBe(true);
    expect(result.current.shouldRenderRail).toBe(true);
  });

  it("does not replay an outgoing bottom observation over a detached tab after its overview resolves", async () => {
    let resolveNextOverview:
      | ((value: SessionOverviewResponse) => void)
      | null = null;
    fetchSessionOverview
      .mockResolvedValueOnce(overview(1, "session-attached"))
      .mockImplementationOnce(
        () =>
          new Promise<SessionOverviewResponse>((resolve) => {
            resolveNextOverview = resolve;
          }),
      );
    const scrollNode = document.createElement("section");
    let scrollTop = 900;
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 100 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: {
        configurable: true,
        get: () => scrollTop,
        set: (nextValue: number) => {
          scrollTop = nextValue;
        },
      },
    });
    const scrollTo = vi.fn((options?: ScrollToOptions | number) => {
      if (typeof options === "object") {
        scrollTop = options.top ?? scrollTop;
      }
    });
    scrollNode.scrollTo = scrollTo as typeof scrollNode.scrollTo;
    const scrollKinds: Array<string | undefined> = [];
    scrollNode.addEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, (event) => {
      scrollKinds.push(
        (event as CustomEvent<{ scrollKind?: string }>).detail?.scrollKind,
      );
    });
    const { result, rerender } = renderHook(
      (props) => useConversationOverviewController(props),
      {
        initialProps: controllerProps({
          scrollContainerRef: { current: scrollNode },
          scrollStateKey: "pane-1:session:session-attached",
          sessionId: "session-attached",
          tailFollowIntent: true,
        }),
      },
    );
    await waitFor(() => expect(result.current.shouldRender).toBe(true));
    scrollTo.mockClear();
    scrollKinds.length = 0;

    rerender(
      controllerProps({
        scrollContainerRef: { current: scrollNode },
        scrollStateKey: "pane-1:session:session-detached",
        sessionId: "session-detached",
        tailFollowIntent: false,
      }),
    );
    await waitFor(() => expect(fetchSessionOverview).toHaveBeenCalledTimes(2));
    expect(scrollTop).toBe(900);

    act(() => resolveNextOverview?.(overview(1, "session-detached")));
    await waitFor(() => expect(result.current.shouldRender).toBe(true));

    expect(scrollTo).not.toHaveBeenCalled();
    expect(scrollKinds).not.toContain("bottom_pin");
    expect(scrollKinds).not.toContain("position_restore");
    expect(scrollTop).toBe(900);
  });

  it("re-derives an attached overview transition from current tail intent", async () => {
    const scrollNode = document.createElement("section");
    let scrollTop = 300;
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 100 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: {
        configurable: true,
        get: () => scrollTop,
        set: (nextValue: number) => {
          scrollTop = nextValue;
        },
      },
    });
    const scrollTo = vi.fn((options?: ScrollToOptions | number) => {
      if (typeof options === "object") {
        scrollTop = options.top ?? scrollTop;
      }
    });
    scrollNode.scrollTo = scrollTo as typeof scrollNode.scrollTo;
    const scrollKinds: Array<string | undefined> = [];
    scrollNode.addEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, (event) => {
      scrollKinds.push(
        (event as CustomEvent<{ scrollKind?: string }>).detail?.scrollKind,
      );
    });

    const { result } = renderHook(() =>
      useConversationOverviewController(
        controllerProps({
          scrollContainerRef: { current: scrollNode },
          tailFollowIntent: true,
        }),
      ),
    );
    await waitFor(() => expect(result.current.shouldRender).toBe(true));

    expect(scrollTop).toBe(900);
    expect(scrollTo).toHaveBeenCalledWith({ top: 900, behavior: "auto" });
    expect(scrollKinds).toContain("bottom_pin");
    expect(scrollKinds).not.toContain("position_restore");
  });

  it("never enables the layout from a stale-session response", async () => {
    let resolveOverview: ((value: SessionOverviewResponse) => void) | null = null;
    fetchSessionOverview.mockImplementationOnce(
      () =>
        new Promise<SessionOverviewResponse>((resolve) => {
          resolveOverview = resolve;
        }),
    );
    fetchSessionOverview.mockImplementationOnce(
      () => new Promise<SessionOverviewResponse>(() => {}),
    );
    const { result, rerender } = renderHook(
      (props) => useConversationOverviewController(props),
      { initialProps: controllerProps() },
    );

    await waitFor(() => expect(fetchSessionOverview).toHaveBeenCalledTimes(1));
    rerender(controllerProps({ sessionId: "session-next" }));
    await waitFor(() => expect(fetchSessionOverview).toHaveBeenCalledTimes(2));
    act(() => resolveOverview?.(overview()));
    await act(async () => Promise.resolve());

    expect(result.current.overview).toBeNull();
    expect(result.current.shouldRender).toBe(false);
    expect(result.current.shouldRenderRail).toBe(true);
  });

  it("keeps a ready same-session rail while refreshing its overview", async () => {
    const { result, rerender } = renderHook(
      (props) => useConversationOverviewController(props),
      { initialProps: controllerProps() },
    );
    await waitFor(() => expect(result.current.shouldRender).toBe(true));
    fetchSessionOverview.mockImplementationOnce(
      () => new Promise<SessionOverviewResponse>(() => {}),
    );

    rerender(controllerProps({ sessionMutationStamp: 2 }));

    expect(result.current.overview).toEqual(overview());
    expect(result.current.shouldRender).toBe(true);
    expect(result.current.shouldRenderRail).toBe(true);
  });

  it("keeps a ready same-session rail after a background refresh fails", async () => {
    const { result, rerender } = renderHook(
      (props) => useConversationOverviewController(props),
      { initialProps: controllerProps() },
    );
    await waitFor(() => expect(result.current.shouldRender).toBe(true));
    const firstOverview = result.current.overview;
    fetchSessionOverview.mockRejectedValueOnce(
      new Error("overview temporarily unavailable"),
    );

    rerender(controllerProps({ sessionMutationStamp: 2 }));

    await waitFor(() => expect(fetchSessionOverview).toHaveBeenCalledTimes(2));
    await act(async () => Promise.resolve());
    expect(result.current.overview).toBe(firstOverview);
    expect(result.current.shouldRender).toBe(true);
    expect(result.current.shouldRenderRail).toBe(true);
  });

  it("does not retain an outgoing overview when the next session refresh fails", async () => {
    const { result, rerender } = renderHook(
      (props) => useConversationOverviewController(props),
      { initialProps: controllerProps() },
    );
    await waitFor(() => expect(result.current.shouldRender).toBe(true));
    fetchSessionOverview.mockRejectedValueOnce(
      new Error("next overview unavailable"),
    );

    rerender(
      controllerProps({
        scrollStateKey: "pane-overview:session:session-next",
        sessionId: "session-next",
      }),
    );

    await waitFor(() => expect(fetchSessionOverview).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(result.current.overview).toBeNull());
    expect(result.current.shouldRender).toBe(false);
    expect(result.current.shouldRenderRail).toBe(true);
  });

  it("recovers from a rejected request on a later activation", async () => {
    fetchSessionOverview.mockRejectedValueOnce(new Error("overview unavailable"));
    const { result, rerender } = renderHook(
      (props) => useConversationOverviewController(props),
      { initialProps: controllerProps() },
    );
    await waitFor(() => expect(fetchSessionOverview).toHaveBeenCalledTimes(1));
    expect(result.current.shouldRender).toBe(false);

    rerender(controllerProps({ isActive: false }));
    rerender(controllerProps({ isActive: true }));

    await waitFor(() => expect(fetchSessionOverview).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(result.current.shouldRender).toBe(true));
    expect(result.current.overview).toEqual(overview());
  });

  it("requests at the message threshold and renders the pending rail immediately", async () => {
    fetchSessionOverview.mockImplementationOnce(
      () => new Promise<SessionOverviewResponse>(() => {}),
    );
    const { result, rerender } = renderHook(
      (props) => useConversationOverviewController(props),
      { initialProps: controllerProps({ messageCount: 29 }) },
    );
    expect(fetchSessionOverview).not.toHaveBeenCalled();
    expect(result.current.shouldRender).toBe(false);

    rerender(controllerProps({ messageCount: 30 }));

    await waitFor(() => expect(fetchSessionOverview).toHaveBeenCalledTimes(1));
    expect(result.current.shouldRender).toBe(false);
    expect(result.current.shouldRenderRail).toBe(true);
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

  it("refreshes the viewport when a same-size resident window shifts", () => {
    fetchSessionOverview.mockImplementationOnce(
      () => new Promise<SessionOverviewResponse>(() => {}),
    );
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, value: 400, writable: true },
    });
    const scrollContainerRef = { current: scrollNode };
    const { result, rerender } = renderHook(
      (props) => useConversationOverviewController(props),
      {
        initialProps: controllerProps({
          messageStartIndex: 69,
          renderedMessageCount: 30,
          scrollContainerRef,
        }),
      },
    );
    expect(result.current.viewport).toEqual({
      startPosition: 81,
      endPosition: 87,
    });

    rerender(
      controllerProps({
        messageStartIndex: 70,
        renderedMessageCount: 30,
        scrollContainerRef,
      }),
    );

    expect(result.current.viewport).toEqual({
      startPosition: 82,
      endPosition: 88,
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
