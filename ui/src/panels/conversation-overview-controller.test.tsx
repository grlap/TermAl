import { render, screen, act } from "@testing-library/react";
import { useEffect, useRef } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useConversationOverviewController } from "./conversation-overview-controller";
import type {
  VirtualizedConversationLayoutSnapshot,
  VirtualizedConversationMessageListHandle,
} from "./VirtualizedConversationMessageList";

function makeLayoutSnapshot(
  sessionId: string,
): VirtualizedConversationLayoutSnapshot {
  return {
    sessionId,
    messageCount: 90,
    estimatedTotalHeightPx: 9_000,
    viewportTopPx: 0,
    viewportHeightPx: 500,
    viewportWidthPx: 1_000,
    isActive: true,
    visiblePageRange: { startIndex: 0, endIndex: 11 },
    mountedPageRange: { startIndex: 0, endIndex: 11 },
    messages: Array.from({ length: 90 }, (_, index) => ({
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

function makeVirtualizerHandle(
  snapshot: VirtualizedConversationLayoutSnapshot,
): VirtualizedConversationMessageListHandle {
  const { messages: _messages, ...viewportSnapshot } = snapshot;
  return {
    getLayoutSnapshot: () => snapshot,
    getViewportSnapshot: () => viewportSnapshot,
    jumpToMessageId: () => false,
    jumpToMessageIndex: () => false,
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
});
