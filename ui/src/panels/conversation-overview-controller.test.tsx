import { render, screen, act } from "@testing-library/react";
import { useEffect, useLayoutEffect, useRef } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useConversationOverviewController } from "./conversation-overview-controller";
import type { ConversationOverviewItem } from "./conversation-overview-map";
import type {
  VirtualizedConversationLayoutSnapshot,
  VirtualizedConversationMessageListHandle,
} from "./VirtualizedConversationMessageList";

function makeLayoutSnapshot(
  sessionId: string,
  messageCount = 90,
): VirtualizedConversationLayoutSnapshot {
  return {
    sessionId,
    messageCount,
    estimatedTotalHeightPx: messageCount * 100,
    viewportTopPx: 0,
    viewportHeightPx: 500,
    viewportWidthPx: 1_000,
    isActive: true,
    visiblePageRange: { startIndex: 0, endIndex: 11 },
    mountedPageRange: { startIndex: 0, endIndex: 11 },
    messages: Array.from({ length: messageCount }, (_, index) => ({
      messageId: `${sessionId}-message-${index}`,
      messageIndex: index,
      pageIndex: Math.floor(index / 8),
      type: "text" as const,
      author: index % 2 === 0 ? ("you" as const) : ("assistant" as const),
      estimatedTopPx: index * 100,
      estimatedHeightPx: 100,
      measuredPageHeightPx: null,
    })),
  };
}

type VirtualizerReadCallbacks = {
  onGetLayoutSnapshot?: () => void;
  onGetViewportSnapshot?: () => void;
};

function makeVirtualizerHandle(
  snapshot: VirtualizedConversationLayoutSnapshot,
  callbacks: VirtualizerReadCallbacks = {},
): VirtualizedConversationMessageListHandle {
  const { messages: _messages, ...viewportSnapshot } = snapshot;
  return {
    getLayoutSnapshot: () => {
      callbacks.onGetLayoutSnapshot?.();
      return snapshot;
    },
    getViewportSnapshot: () => {
      callbacks.onGetViewportSnapshot?.();
      return viewportSnapshot;
    },
    jumpToMessageId: () => false,
    jumpToMessageIndex: () => false,
  };
}

function makeOverviewItem(
  overrides: Partial<ConversationOverviewItem> = {},
): ConversationOverviewItem {
  return {
    messageId: "session-a-message-10",
    messageIndex: 10,
    type: "text",
    author: "assistant",
    kind: "assistant_text",
    status: null,
    estimatedTopPx: 1_000,
    estimatedHeightPx: 100,
    measuredHeightPx: null,
    measuredPageHeightPx: null,
    documentTopPx: 1_000,
    documentHeightPx: 100,
    mapTopPx: 100,
    mapHeightPx: 10,
    markerIds: [],
    markers: [],
    textSample: "message 10",
    ...overrides,
  };
}

function OverviewControllerHarness({ sessionId }: { sessionId: string }) {
  const scrollContainerRef = useRef<HTMLElement | null>(
    document.createElement("section"),
  );
  const overview = useConversationOverviewController({
    agent: "Codex",
    isActive: true,
    messageCount: 90,
    scrollContainerRef,
    sessionId,
    showWaitingIndicator: false,
    waitingIndicatorPrompt: null,
  });

  useEffect(() => {
    overview.virtualizerHandleRef.current = makeVirtualizerHandle(
      makeLayoutSnapshot(sessionId),
    );
    return () => {
      overview.virtualizerHandleRef.current = null;
    };
  }, [overview.virtualizerHandleRef, sessionId]);

  return (
    <output data-testid={`overview-${sessionId}`}>
      {overview.shouldRenderRail ? "ready" : "pending"}
    </output>
  );
}

function OverviewNavigateHarness({
  handle,
  item,
  onFullTranscriptDemand,
}: {
  handle: VirtualizedConversationMessageListHandle;
  item: ConversationOverviewItem;
  onFullTranscriptDemand: () => void;
}) {
  const scrollContainerRef = useRef<HTMLElement | null>(
    document.createElement("section"),
  );
  const overview = useConversationOverviewController({
    agent: "Codex",
    isActive: true,
    messageCount: 10,
    onFullTranscriptDemand,
    scrollContainerRef,
    sessionId: "session-a",
    showWaitingIndicator: false,
    waitingIndicatorPrompt: null,
  });

  useEffect(() => {
    overview.virtualizerHandleRef.current = handle;
    return () => {
      overview.virtualizerHandleRef.current = null;
    };
  }, [handle, overview.virtualizerHandleRef]);

  return (
    <button type="button" onClick={() => overview.navigate(item)}>
      Navigate
    </button>
  );
}

function OverviewGrowthHarness({
  messageCount,
  readCallbacks,
}: {
  messageCount: number;
  readCallbacks?: VirtualizerReadCallbacks;
}) {
  const scrollContainerRef = useRef<HTMLElement | null>(
    document.createElement("section"),
  );
  const snapshot = makeLayoutSnapshot("session-a", messageCount);
  const overview = useConversationOverviewController({
    agent: "Codex",
    isActive: true,
    messageCount,
    scrollContainerRef,
    sessionId: "session-a",
    showWaitingIndicator: false,
    waitingIndicatorPrompt: null,
  });

  useLayoutEffect(() => {
    overview.virtualizerHandleRef.current = makeVirtualizerHandle(
      snapshot,
      readCallbacks,
    );
    return () => {
      overview.virtualizerHandleRef.current = null;
    };
  }, [overview.virtualizerHandleRef, readCallbacks, snapshot]);

  return (
    <output data-testid="layout-message-count">
      {overview.layoutSnapshot?.messageCount ?? "none"}
    </output>
  );
}

describe("useConversationOverviewController", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("activates long-session rails from idle work after transcript paint frames", () => {
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const idleWindow = window as Window &
      typeof globalThis & {
        requestIdleCallback?: (
          callback: IdleRequestCallback,
          options?: IdleRequestOptions,
        ) => number;
        cancelIdleCallback?: (handle: number) => void;
      };
    const originalRequestIdleCallback = idleWindow.requestIdleCallback;
    const originalCancelIdleCallback = idleWindow.cancelIdleCallback;
    const frameCallbacks = new Map<number, FrameRequestCallback>();
    const idleCallbacks = new Map<number, IdleRequestCallback>();
    let nextFrameId = 1;
    let nextIdleId = 1;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      frameCallbacks.set(frameId, callback);
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = ((frameId: number) => {
      frameCallbacks.delete(frameId);
    }) as typeof cancelAnimationFrame;
    idleWindow.requestIdleCallback = ((callback: IdleRequestCallback) => {
      const idleId = nextIdleId;
      nextIdleId += 1;
      idleCallbacks.set(idleId, callback);
      return idleId;
    }) as typeof requestIdleCallback;
    idleWindow.cancelIdleCallback = ((idleId: number) => {
      idleCallbacks.delete(idleId);
    }) as typeof cancelIdleCallback;
    const flushNextFrame = () => {
      const nextFrame = frameCallbacks.entries().next().value as
        | [number, FrameRequestCallback]
        | undefined;
      expect(nextFrame).toBeDefined();
      if (!nextFrame) {
        return;
      }
      const [frameId, callback] = nextFrame;
      frameCallbacks.delete(frameId);
      callback(performance.now());
    };
    const flushNextIdle = () => {
      const nextIdle = idleCallbacks.entries().next().value as
        | [number, IdleRequestCallback]
        | undefined;
      expect(nextIdle).toBeDefined();
      if (!nextIdle) {
        return;
      }
      const [idleId, callback] = nextIdle;
      idleCallbacks.delete(idleId);
      callback({
        didTimeout: false,
        timeRemaining: () => 16,
      });
    };

    try {
      render(
        <>
          <OverviewControllerHarness sessionId="session-a" />
          <OverviewControllerHarness sessionId="session-b" />
        </>,
      );

      expect(screen.getByTestId("overview-session-a")).toHaveTextContent(
        "pending",
      );
      expect(screen.getByTestId("overview-session-b")).toHaveTextContent(
        "pending",
      );

      act(flushNextFrame);
      act(flushNextFrame);
      act(flushNextFrame);
      act(flushNextFrame);

      expect(screen.getByTestId("overview-session-a")).toHaveTextContent(
        "pending",
      );
      expect(screen.getByTestId("overview-session-b")).toHaveTextContent(
        "pending",
      );

      act(flushNextIdle);

      expect(screen.getByTestId("overview-session-a")).toHaveTextContent(
        "ready",
      );
      expect(screen.getByTestId("overview-session-b")).toHaveTextContent(
        "pending",
      );

      act(flushNextIdle);

      expect(screen.getByTestId("overview-session-a")).toHaveTextContent(
        "ready",
      );
      expect(screen.getByTestId("overview-session-b")).toHaveTextContent(
        "ready",
      );
    } finally {
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      idleWindow.requestIdleCallback = originalRequestIdleCallback;
      idleWindow.cancelIdleCallback = originalCancelIdleCallback;
    }
  });

  it("coalesces ready layout refreshes when the transcript message count grows", () => {
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const idleWindow = window as Window &
      typeof globalThis & {
        requestIdleCallback?: (
          callback: IdleRequestCallback,
          options?: IdleRequestOptions,
        ) => number;
        cancelIdleCallback?: (handle: number) => void;
      };
    const originalRequestIdleCallback = idleWindow.requestIdleCallback;
    const originalCancelIdleCallback = idleWindow.cancelIdleCallback;
    const frameCallbacks = new Map<number, FrameRequestCallback>();
    const idleCallbacks = new Map<number, IdleRequestCallback>();
    let nextFrameId = 1;
    let nextIdleId = 1;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      frameCallbacks.set(frameId, callback);
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = ((frameId: number) => {
      frameCallbacks.delete(frameId);
    }) as typeof cancelAnimationFrame;
    idleWindow.requestIdleCallback = ((callback: IdleRequestCallback) => {
      const idleId = nextIdleId;
      nextIdleId += 1;
      idleCallbacks.set(idleId, callback);
      return idleId;
    }) as typeof requestIdleCallback;
    idleWindow.cancelIdleCallback = ((idleId: number) => {
      idleCallbacks.delete(idleId);
    }) as typeof cancelIdleCallback;
    const flushNextFrame = () => {
      const nextFrame = frameCallbacks.entries().next().value as
        | [number, FrameRequestCallback]
        | undefined;
      expect(nextFrame).toBeDefined();
      if (!nextFrame) {
        return;
      }
      const [frameId, callback] = nextFrame;
      frameCallbacks.delete(frameId);
      callback(performance.now());
    };
    const flushNextIdle = () => {
      const nextIdle = idleCallbacks.entries().next().value as
        | [number, IdleRequestCallback]
        | undefined;
      expect(nextIdle).toBeDefined();
      if (!nextIdle) {
        return;
      }
      const [idleId, callback] = nextIdle;
      idleCallbacks.delete(idleId);
      callback({
        didTimeout: false,
        timeRemaining: () => 16,
      });
    };

    try {
      const readCallbacks = {
        onGetLayoutSnapshot: vi.fn(),
        onGetViewportSnapshot: vi.fn(),
      };
      const { rerender } = render(
        <OverviewGrowthHarness
          messageCount={90}
          readCallbacks={readCallbacks}
        />,
      );

      expect(screen.getByTestId("layout-message-count")).toHaveTextContent(
        "none",
      );

      act(flushNextFrame);
      act(flushNextFrame);
      act(flushNextIdle);

      expect(screen.getByTestId("layout-message-count")).toHaveTextContent(
        "90",
      );
      expect(readCallbacks.onGetLayoutSnapshot).toHaveBeenCalledTimes(1);
      expect(readCallbacks.onGetViewportSnapshot).not.toHaveBeenCalled();
      expect(frameCallbacks.size).toBe(1);

      for (const messageCount of [
        120, 140, 160, 180, 200, 220, 240, 260, 280, 300,
      ]) {
        act(() => {
          rerender(
            <OverviewGrowthHarness
              messageCount={messageCount}
              readCallbacks={readCallbacks}
            />,
          );
        });

        expect(screen.getByTestId("layout-message-count")).toHaveTextContent(
          "90",
        );
        expect(frameCallbacks.size).toBe(1);
        expect(readCallbacks.onGetLayoutSnapshot).toHaveBeenCalledTimes(1);
        expect(readCallbacks.onGetViewportSnapshot).not.toHaveBeenCalled();
      }

      act(flushNextFrame);

      expect(screen.getByTestId("layout-message-count")).toHaveTextContent(
        "300",
      );
      expect(readCallbacks.onGetLayoutSnapshot).toHaveBeenCalledTimes(2);
      expect(readCallbacks.onGetViewportSnapshot).not.toHaveBeenCalled();
      expect(frameCallbacks.size).toBe(0);
    } finally {
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      idleWindow.requestIdleCallback = originalRequestIdleCallback;
      idleWindow.cancelIdleCallback = originalCancelIdleCallback;
    }
  });

  it("cancels delayed rail activation when the transcript drops below the threshold", () => {
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const idleWindow = window as Window &
      typeof globalThis & {
        requestIdleCallback?: (
          callback: IdleRequestCallback,
          options?: IdleRequestOptions,
        ) => number;
        cancelIdleCallback?: (handle: number) => void;
      };
    const originalRequestIdleCallback = idleWindow.requestIdleCallback;
    const originalCancelIdleCallback = idleWindow.cancelIdleCallback;
    const frameCallbacks = new Map<number, FrameRequestCallback>();
    const idleCallbacks = new Map<number, IdleRequestCallback>();
    let nextFrameId = 1;
    let nextIdleId = 1;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      frameCallbacks.set(frameId, callback);
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = ((frameId: number) => {
      frameCallbacks.delete(frameId);
    }) as typeof cancelAnimationFrame;
    idleWindow.requestIdleCallback = ((callback: IdleRequestCallback) => {
      const idleId = nextIdleId;
      nextIdleId += 1;
      idleCallbacks.set(idleId, callback);
      return idleId;
    }) as typeof requestIdleCallback;
    idleWindow.cancelIdleCallback = ((idleId: number) => {
      idleCallbacks.delete(idleId);
    }) as typeof cancelIdleCallback;
    const flushNextFrame = () => {
      const nextFrame = frameCallbacks.entries().next().value as
        | [number, FrameRequestCallback]
        | undefined;
      expect(nextFrame).toBeDefined();
      if (!nextFrame) {
        return;
      }
      const [frameId, callback] = nextFrame;
      frameCallbacks.delete(frameId);
      callback(performance.now());
    };

    try {
      const { rerender } = render(<OverviewGrowthHarness messageCount={90} />);

      act(flushNextFrame);
      expect(frameCallbacks.size).toBe(1);

      act(() => {
        rerender(<OverviewGrowthHarness messageCount={1} />);
      });

      expect(screen.getByTestId("layout-message-count")).toHaveTextContent(
        "none",
      );
      expect(frameCallbacks.size).toBe(0);
      expect(idleCallbacks.size).toBe(0);
    } finally {
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      idleWindow.requestIdleCallback = originalRequestIdleCallback;
      idleWindow.cancelIdleCallback = originalCancelIdleCallback;
    }
  });

  it("retries full-transcript navigation on a second frame when hydration has not committed", () => {
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const frameCallbacks = new Map<number, FrameRequestCallback>();
    let nextFrameId = 1;
    const flushNextFrame = () => {
      const nextFrame = frameCallbacks.entries().next().value as
        | [number, FrameRequestCallback]
        | undefined;
      expect(nextFrame).toBeDefined();
      if (!nextFrame) {
        return;
      }
      const [frameId, callback] = nextFrame;
      frameCallbacks.delete(frameId);
      callback(performance.now());
    };
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      frameCallbacks.set(frameId, callback);
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = ((frameId: number) => {
      frameCallbacks.delete(frameId);
    }) as typeof cancelAnimationFrame;

    const onFullTranscriptDemand = vi.fn();
    const jumpToMessageId = vi
      .fn<VirtualizedConversationMessageListHandle["jumpToMessageId"]>()
      .mockReturnValueOnce(false)
      .mockReturnValueOnce(false)
      .mockReturnValueOnce(true);
    const jumpToMessageIndex = vi
      .fn<VirtualizedConversationMessageListHandle["jumpToMessageIndex"]>()
      .mockReturnValue(false);
    const snapshot = makeLayoutSnapshot("session-a");
    const { messages: _messages, ...viewportSnapshot } = snapshot;
    const handle: VirtualizedConversationMessageListHandle = {
      getLayoutSnapshot: () => snapshot,
      getViewportSnapshot: () => viewportSnapshot,
      jumpToMessageId,
      jumpToMessageIndex,
    };

    try {
      render(
        <OverviewNavigateHarness
          handle={handle}
          item={makeOverviewItem()}
          onFullTranscriptDemand={onFullTranscriptDemand}
        />,
      );

      act(() => {
        screen.getByRole("button", { name: "Navigate" }).click();
      });

      expect(onFullTranscriptDemand).toHaveBeenCalledTimes(1);
      expect(jumpToMessageId).toHaveBeenCalledTimes(1);

      act(flushNextFrame);
      expect(jumpToMessageId).toHaveBeenCalledTimes(2);
      expect(jumpToMessageIndex).toHaveBeenCalledTimes(2);

      act(flushNextFrame);
      expect(jumpToMessageId).toHaveBeenCalledTimes(3);
      expect(jumpToMessageIndex).toHaveBeenCalledTimes(2);
      expect(onFullTranscriptDemand).toHaveBeenCalledTimes(1);

      act(flushNextFrame);
      expect(handle.getViewportSnapshot()).toEqual(viewportSnapshot);
      expect(frameCallbacks.size).toBe(0);
    } finally {
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
    }
  });

  it("does not use message-index fallback when the snapshot index belongs to another message", () => {
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const frameCallbacks = new Map<number, FrameRequestCallback>();
    let nextFrameId = 1;
    const flushNextFrame = () => {
      const nextFrame = frameCallbacks.entries().next().value as
        | [number, FrameRequestCallback]
        | undefined;
      expect(nextFrame).toBeDefined();
      if (!nextFrame) {
        return;
      }
      const [frameId, callback] = nextFrame;
      frameCallbacks.delete(frameId);
      callback(performance.now());
    };
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      frameCallbacks.set(frameId, callback);
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = ((frameId: number) => {
      frameCallbacks.delete(frameId);
    }) as typeof cancelAnimationFrame;

    const onFullTranscriptDemand = vi.fn();
    const jumpToMessageId = vi
      .fn<VirtualizedConversationMessageListHandle["jumpToMessageId"]>()
      .mockReturnValue(false);
    const jumpToMessageIndex = vi
      .fn<VirtualizedConversationMessageListHandle["jumpToMessageIndex"]>()
      .mockReturnValue(true);
    const snapshot = makeLayoutSnapshot("session-a");
    const tailSnapshot: VirtualizedConversationLayoutSnapshot = {
      ...snapshot,
      messages: snapshot.messages.map((message) =>
        message.messageIndex === 10
          ? { ...message, messageId: "session-a-tail-message-10" }
          : message,
      ),
    };
    const { messages: _messages, ...viewportSnapshot } = tailSnapshot;
    const handle: VirtualizedConversationMessageListHandle = {
      getLayoutSnapshot: () => tailSnapshot,
      getViewportSnapshot: () => viewportSnapshot,
      jumpToMessageId,
      jumpToMessageIndex,
    };

    try {
      render(
        <OverviewNavigateHarness
          handle={handle}
          item={makeOverviewItem()}
          onFullTranscriptDemand={onFullTranscriptDemand}
        />,
      );

      act(() => {
        screen.getByRole("button", { name: "Navigate" }).click();
      });

      expect(onFullTranscriptDemand).toHaveBeenCalledTimes(1);
      expect(jumpToMessageId).toHaveBeenCalledTimes(1);
      expect(jumpToMessageIndex).not.toHaveBeenCalled();

      act(flushNextFrame);
      expect(jumpToMessageId).toHaveBeenCalledTimes(2);
      expect(jumpToMessageIndex).not.toHaveBeenCalled();

      act(flushNextFrame);
      expect(jumpToMessageId).toHaveBeenCalledTimes(3);
      expect(jumpToMessageIndex).not.toHaveBeenCalled();
      expect(onFullTranscriptDemand).toHaveBeenCalledTimes(1);
      expect(frameCallbacks.size).toBe(0);
    } finally {
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
    }
  });
});
