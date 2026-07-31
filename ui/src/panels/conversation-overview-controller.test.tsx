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
  overrides: Partial<Parameters<typeof useConversationOverviewController>[0]> = {},
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
