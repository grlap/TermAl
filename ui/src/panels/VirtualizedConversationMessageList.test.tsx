import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import type { RefObject } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS,
  VirtualizedConversationMessageList,
  type VirtualizedConversationMessageListHandleRef,
} from "./VirtualizedConversationMessageList";
import {
  VIRTUALIZED_MESSAGE_GAP_PX,
  buildVirtualizedMessageLayout,
  estimateConversationMessageHeight,
} from "./conversation-virtualization";
import {
  DEFERRED_RENDER_RESUME_EVENT,
  DEFERRED_RENDER_SUSPENDED_ATTRIBUTE,
} from "../deferred-render";
import { notifyMessageStackScrollWrite } from "../message-stack-scroll-sync";
import type { Message } from "../types";

function makeTextMessages(count: number): Message[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `message-${index + 1}`,
    type: "text",
    timestamp: `10:${String(index).padStart(2, "0")}`,
    author: index % 2 === 0 ? "you" : "assistant",
    text: `Message ${index + 1}`,
  }));
}

function makeDomRect({
  top = 0,
  height = 0,
  width = 100,
}: {
  top?: number;
  height?: number;
  width?: number;
}) {
  return {
    bottom: top + height,
    height,
    left: 0,
    right: width,
    top,
    width,
    x: 0,
    y: top,
    toJSON: () => ({}),
  } as DOMRect;
}

type VirtualizedHarnessOptions = {
  clientHeight?: number;
  clientWidth?: number;
  initialScrollTop?: number;
  messages: Message[];
  renderMessageCard?: (
    message: Message,
    preferImmediateHeavyRender: boolean,
  ) => JSX.Element;
  conversationSearchQuery?: string;
  conversationSearchMatchedItemKeys?: ReadonlySet<string>;
  conversationSearchActiveItemKey?: string | null;
  scrollHeight?: () => number;
  slotRect?: (
    message: Message,
    messageIndex: number,
    scrollTop: number,
  ) => { height: number; top: number } | null;
  slotHeight?: (message: Message) => number;
  virtualizerHandleRef?: VirtualizedConversationMessageListHandleRef;
};

type VirtualizedSearchOptions = Pick<
  VirtualizedHarnessOptions,
  | "conversationSearchActiveItemKey"
  | "conversationSearchMatchedItemKeys"
  | "conversationSearchQuery"
>;

function renderVirtualizedHarness({
  clientHeight = 500,
  clientWidth = 1000,
  initialScrollTop = 0,
  messages,
  renderMessageCard = (message) => (
    <article className="message-card">{message.id}</article>
  ),
  conversationSearchQuery = "",
  conversationSearchMatchedItemKeys = new Set(),
  conversationSearchActiveItemKey = null,
  scrollHeight,
  slotRect,
  slotHeight = () => 80,
  virtualizerHandleRef,
}: VirtualizedHarnessOptions) {
  const OriginalResizeObserver = window.ResizeObserver;
  const originalRequestAnimationFrame = window.requestAnimationFrame;
  const originalCancelAnimationFrame = window.cancelAnimationFrame;
  const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
  const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
  let nextFrameId = 1;
  let scrollTop = initialScrollTop;
  const scrollWrites: number[] = [];
  const buildEstimatedLayout = (nextMessages: Message[]) =>
    buildVirtualizedMessageLayout(
      nextMessages.map((message) =>
        estimateConversationMessageHeight(message, {
          availableWidthPx: clientWidth,
        }),
      ),
    );
  let currentMessages = messages;
  let estimatedLayout = buildEstimatedLayout(currentMessages);
  const resolvedScrollHeight =
    scrollHeight ?? (() => estimatedLayout.totalHeight);

  const setCurrentMessages = (nextMessages: Message[]) => {
    currentMessages = nextMessages;
    estimatedLayout = buildEstimatedLayout(currentMessages);
  };

  class ResizeObserverMock {
    constructor(private readonly callback: ResizeObserverCallback) {}
    observe(target: Element) {
      resizeCallbacks.set(target, this.callback);
    }
    disconnect() {}
  }

  const scrollNode = document.createElement("div");
  Object.defineProperty(scrollNode, "clientHeight", {
    configurable: true,
    get: () => clientHeight,
  });
  Object.defineProperty(scrollNode, "clientWidth", {
    configurable: true,
    get: () => clientWidth,
  });
  Object.defineProperty(scrollNode, "scrollHeight", {
    configurable: true,
    get: resolvedScrollHeight,
  });
  Object.defineProperty(scrollNode, "scrollTop", {
    configurable: true,
    get: () => scrollTop,
    set: (nextValue: number) => {
      scrollTop = nextValue;
      scrollWrites.push(nextValue);
    },
  });

  window.ResizeObserver =
    ResizeObserverMock as unknown as typeof ResizeObserver;
  window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
    const frameId = nextFrameId;
    nextFrameId += 1;
    queueMicrotask(() => callback(performance.now()));
    return frameId;
  }) as typeof requestAnimationFrame;
  window.cancelAnimationFrame =
    vi.fn() as unknown as typeof cancelAnimationFrame;
  Element.prototype.getBoundingClientRect =
    function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      if (element === scrollNode) {
        return makeDomRect({ height: clientHeight, width: clientWidth });
      }
      if (element.classList.contains("virtualized-message-slot")) {
        const messageIndex = currentMessages.findIndex(
          (candidate) => candidate.id === element.dataset.messageId,
        );
        const message = messageIndex >= 0 ? currentMessages[messageIndex] : undefined;
        if (message) {
          const customRect = slotRect?.(message, messageIndex, scrollTop);
          if (customRect) {
            return makeDomRect(customRect);
          }
        }
        const height = message ? slotHeight(message) : 80;
        return makeDomRect({
          top:
            messageIndex >= 0
              ? messageIndex * (height + VIRTUALIZED_MESSAGE_GAP_PX) - scrollTop
              : 0,
          height,
        });
      }
      return makeDomRect({ height: clientHeight, width: clientWidth });
    };
  const scrollContainerRef = {
    current: scrollNode,
  } as RefObject<HTMLElement | null>;

  const restore = () => {
    window.ResizeObserver = OriginalResizeObserver;
    window.requestAnimationFrame = originalRequestAnimationFrame;
    window.cancelAnimationFrame = originalCancelAnimationFrame;
    Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
  };

  const renderList = (searchOptions: VirtualizedSearchOptions = {}) => (
    <VirtualizedConversationMessageList
      isActive
      renderMessageCard={renderMessageCard}
      sessionId="session-a"
      messages={currentMessages}
      scrollContainerRef={scrollContainerRef}
      onApprovalDecision={() => {}}
      onUserInputSubmit={() => {}}
      onMcpElicitationSubmit={() => {}}
      onCodexAppRequestSubmit={() => {}}
      conversationSearchQuery={
        searchOptions.conversationSearchQuery ?? conversationSearchQuery
      }
      conversationSearchMatchedItemKeys={
        searchOptions.conversationSearchMatchedItemKeys ??
        conversationSearchMatchedItemKeys
      }
      conversationSearchActiveItemKey={
        "conversationSearchActiveItemKey" in searchOptions
          ? searchOptions.conversationSearchActiveItemKey
          : conversationSearchActiveItemKey
      }
      virtualizerHandleRef={virtualizerHandleRef}
    />
  );
  const result = render(renderList());

  return {
    ...result,
    get estimatedLayout() {
      return estimatedLayout;
    },
    get scrollTop() {
      return scrollTop;
    },
    resizeCallbacks,
    restore,
    scrollNode,
    scrollWrites,
    rerenderWithMessages(
      nextMessages: Message[],
      nextSearchOptions: VirtualizedSearchOptions = {},
    ) {
      setCurrentMessages(nextMessages);
      result.rerender(renderList(nextSearchOptions));
    },
    rerenderWithSearch(nextSearchOptions: VirtualizedSearchOptions) {
      result.rerender(renderList(nextSearchOptions));
    },
    setScrollTop(nextValue: number) {
      scrollTop = nextValue;
    },
  };
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

async function advanceIdleMountedRangeCompaction() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(
      VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS + 1,
    );
  });
}

describe("VirtualizedConversationMessageList foundation", () => {
  it("exposes a layout snapshot and explicit jump helpers", async () => {
    const messages = makeTextMessages(48);
    const virtualizerHandleRef: VirtualizedConversationMessageListHandleRef = {
      current: null,
    };
    const harness = renderVirtualizedHarness({
      messages,
      virtualizerHandleRef,
    });

    try {
      await waitFor(() => {
        expect(virtualizerHandleRef.current).not.toBeNull();
      });

      const initialSnapshot = virtualizerHandleRef.current!.getLayoutSnapshot();
      expect(initialSnapshot.sessionId).toBe("session-a");
      expect(initialSnapshot.messageCount).toBe(messages.length);
      expect(initialSnapshot.messages[0]).toMatchObject({
        messageId: "message-1",
        messageIndex: 0,
        pageIndex: 0,
      });
      expect(
        initialSnapshot.messages[initialSnapshot.messages.length - 1],
      ).toMatchObject({
        messageId: "message-48",
        messageIndex: 47,
      });

      act(() => {
        expect(
          virtualizerHandleRef.current!.jumpToMessageIndex(0, {
            align: "start",
            flush: true,
          }),
        ).toBe(true);
      });

      expect(harness.scrollTop).toBe(0);
      expect(screen.getByText("message-1")).toBeInTheDocument();

      act(() => {
        expect(
          virtualizerHandleRef.current!.jumpToMessageId("message-32", {
            align: "center",
            flush: true,
          }),
        ).toBe(true);
      });

      await waitFor(() => {
        expect(screen.getByText("message-32")).toBeInTheDocument();
      });
      expect(harness.scrollTop).toBeGreaterThan(0);
      expect(virtualizerHandleRef.current!.jumpToMessageIndex(-1)).toBe(false);
      expect(virtualizerHandleRef.current!.jumpToMessageId("missing-message")).toBe(
        false,
      );
    } finally {
      harness.restore();
    }
  });

  it("updates the heavy-render preference immediately when direct scroll input starts", async () => {
    const messages = makeTextMessages(4);
    const preferImmediateValues: boolean[] = [];
    const harness = renderVirtualizedHarness({
      messages,
      renderMessageCard: (message, preferImmediateHeavyRender) => {
        preferImmediateValues.push(preferImmediateHeavyRender);
        return <article className="message-card">{message.id}</article>;
      },
    });

    try {
      await waitFor(() => {
        expect(preferImmediateValues[preferImmediateValues.length - 1]).toBe(
          true,
        );
      });

      act(() => {
        fireEvent.wheel(harness.scrollNode, { deltaY: 48 });
      });

      await waitFor(() => {
        expect(preferImmediateValues[preferImmediateValues.length - 1]).toBe(
          false,
        );
      });
    } finally {
      harness.restore();
    }
  });

  it("suspends and resumes deferred rendering from the production scroll-input path", async () => {
    vi.useFakeTimers();
    const resumeListener = vi.fn();
    const harness = renderVirtualizedHarness({
      messages: makeTextMessages(4),
    });
    harness.scrollNode.addEventListener(
      DEFERRED_RENDER_RESUME_EVENT,
      resumeListener,
    );

    try {
      await act(async () => {
        await Promise.resolve();
      });

      act(() => {
        fireEvent.wheel(harness.scrollNode, { deltaY: 48 });
      });

      expect(
        harness.scrollNode.getAttribute(DEFERRED_RENDER_SUSPENDED_ATTRIBUTE),
      ).toBe("true");

      act(() => {
        vi.advanceTimersByTime(10);
      });

      expect(
        harness.scrollNode.getAttribute(DEFERRED_RENDER_SUSPENDED_ATTRIBUTE),
      ).toBeNull();
      expect(resumeListener).toHaveBeenCalledTimes(1);
    } finally {
      harness.scrollNode.removeEventListener(
        DEFERRED_RENDER_RESUME_EVENT,
        resumeListener,
      );
      harness.restore();
    }
  });

  it("keeps bottom-follow native smooth-scroll ticks out of user-scroll cooldown", async () => {
    let measuredSlotHeight = 180;
    const messages = makeTextMessages(3);
    const harness = renderVirtualizedHarness({
      clientHeight: 100,
      initialScrollTop: 400,
      messages,
      scrollHeight: () => 500 + (measuredSlotHeight - 180),
      slotHeight: () => measuredSlotHeight,
    });

    try {
      const slot = await waitFor(() => {
        const candidate = harness.container.querySelector(
          ".virtualized-message-slot",
        );
        expect(candidate).not.toBeNull();
        expect(harness.resizeCallbacks.has(candidate!)).toBe(true);
        return candidate!;
      });

      act(() => {
        notifyMessageStackScrollWrite(harness.scrollNode, {
          scrollKind: "bottom_follow",
        });
      });

      act(() => {
        harness.setScrollTop(410);
        fireEvent.scroll(harness.scrollNode);
        harness.setScrollTop(420);
        fireEvent.scroll(harness.scrollNode);
      });

      harness.scrollWrites.length = 0;
      measuredSlotHeight = 260;
      await act(async () => {
        harness.resizeCallbacks.get(slot)?.(
          [] as unknown as ResizeObserverEntry[],
          {} as ResizeObserver,
        );
        await Promise.resolve();
        await Promise.resolve();
      });

      expect(harness.scrollWrites).toContain(480);
    } finally {
      harness.restore();
    }
  });

  it("rebuilds the rendered window after manual scroll starts from an active search result", async () => {
    const messages = makeTextMessages(160);
    const matchedKeys = new Set(messages.map((message) => `message:${message.id}`));
    const harness = renderVirtualizedHarness({
      messages,
      conversationSearchQuery: "Message",
      conversationSearchMatchedItemKeys: matchedKeys,
      conversationSearchActiveItemKey: "message:message-140",
    });

    try {
      await waitFor(() => {
        expect(screen.getByText("message-140")).toBeInTheDocument();
      });
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();

      vi.useFakeTimers();
      act(() => {
        harness.setScrollTop(0);
        fireEvent.scroll(harness.scrollNode);
      });

      expect(screen.getByText("message-1")).toBeInTheDocument();

      await advanceIdleMountedRangeCompaction();
      expect(screen.queryByText("message-140")).not.toBeInTheDocument();
    } finally {
      harness.restore();
    }
  });

  it("keeps a released search pin released for same-result query changes and rearms on active-result changes", async () => {
    const messages = makeTextMessages(160);
    const matchedKeys = new Set(messages.map((message) => `message:${message.id}`));
    const harness = renderVirtualizedHarness({
      messages,
      conversationSearchQuery: "Message",
      conversationSearchMatchedItemKeys: matchedKeys,
      conversationSearchActiveItemKey: "message:message-140",
    });

    try {
      await waitFor(() => {
        expect(screen.getByText("message-140")).toBeInTheDocument();
      });

      act(() => {
        harness.setScrollTop(0);
        fireEvent.scroll(harness.scrollNode);
      });
      await waitFor(() => {
        expect(screen.getByText("message-1")).toBeInTheDocument();
      });
      expect(screen.queryByText("message-140")).not.toBeInTheDocument();
      expect(harness.scrollTop).toBe(0);

      act(() => {
        harness.rerenderWithSearch({
          conversationSearchQuery: "Message 140",
          conversationSearchMatchedItemKeys: matchedKeys,
          conversationSearchActiveItemKey: "message:message-140",
        });
      });
      await waitFor(() => {
        expect(screen.getByText("message-1")).toBeInTheDocument();
      });
      expect(screen.queryByText("message-140")).not.toBeInTheDocument();
      expect(harness.scrollTop).toBe(0);

      act(() => {
        harness.setScrollTop(0);
        fireEvent.scroll(harness.scrollNode);
      });
      await waitFor(() => {
        expect(screen.getByText("message-1")).toBeInTheDocument();
      });

      act(() => {
        harness.rerenderWithSearch({
          conversationSearchQuery: "Message",
          conversationSearchMatchedItemKeys: matchedKeys,
          conversationSearchActiveItemKey: "message:message-120",
        });
      });
      await waitFor(() => {
        expect(screen.getByText("message-120")).toBeInTheDocument();
      });

      act(() => {
        harness.rerenderWithSearch({
          conversationSearchQuery: "Message",
          conversationSearchMatchedItemKeys: matchedKeys,
          conversationSearchActiveItemKey: "message:message-140",
        });
      });
      await waitFor(() => {
        expect(screen.getByText("message-140")).toBeInTheDocument();
      });
    } finally {
      harness.restore();
    }
  });

  it("does not recenter the same active search match while the query is refined", async () => {
    const messages = makeTextMessages(160);
    const matchedKeys = new Set(messages.map((message) => `message:${message.id}`));
    const harness = renderVirtualizedHarness({
      messages,
      conversationSearchQuery: "Message",
      conversationSearchMatchedItemKeys: matchedKeys,
      conversationSearchActiveItemKey: "message:message-140",
    });

    try {
      await waitFor(() => {
        expect(screen.getByText("message-140")).toBeInTheDocument();
        expect(harness.scrollTop).toBeGreaterThanOrEqual(18_000);
      });
      const scrollTopAfterInitialPin = harness.scrollTop;
      const scrollWriteCountAfterInitialPin = harness.scrollWrites.length;

      act(() => {
        harness.rerenderWithSearch({
          conversationSearchQuery: "Message 14",
          conversationSearchMatchedItemKeys: matchedKeys,
          conversationSearchActiveItemKey: "message:message-140",
        });
      });

      await waitFor(() => {
        expect(screen.getByText("message-140")).toBeInTheDocument();
      });
      expect(harness.scrollTop).toBe(scrollTopAfterInitialPin);
      expect(harness.scrollWrites).toHaveLength(scrollWriteCountAfterInitialPin);
    } finally {
      harness.restore();
    }
  });

  it("rebuilds from a user-owned scroll write after an active search result", async () => {
    const messages = makeTextMessages(160);
    const matchedKeys = new Set(messages.map((message) => `message:${message.id}`));
    const harness = renderVirtualizedHarness({
      messages,
      conversationSearchQuery: "Message",
      conversationSearchMatchedItemKeys: matchedKeys,
      conversationSearchActiveItemKey: "message:message-140",
    });

    try {
      await waitFor(() => {
        expect(screen.getByText("message-140")).toBeInTheDocument();
      });
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();

      vi.useFakeTimers();
      act(() => {
        harness.setScrollTop(0);
        notifyMessageStackScrollWrite(harness.scrollNode, {
          scrollKind: "page_jump",
          scrollSource: "user",
        });
      });

      expect(screen.getByText("message-1")).toBeInTheDocument();

      await advanceIdleMountedRangeCompaction();
      expect(screen.queryByText("message-140")).not.toBeInTheDocument();
    } finally {
      harness.restore();
    }
  });

  it("releases an active search pin from explicit wheel intent before a programmatic scroll write", async () => {
    const messages = makeTextMessages(160);
    const matchedKeys = new Set(messages.map((message) => `message:${message.id}`));
    const harness = renderVirtualizedHarness({
      messages,
      conversationSearchQuery: "Message",
      conversationSearchMatchedItemKeys: matchedKeys,
      conversationSearchActiveItemKey: "message:message-140",
    });

    try {
      await waitFor(() => {
        expect(screen.getByText("message-140")).toBeInTheDocument();
      });
      expect(screen.queryByText("message-1")).not.toBeInTheDocument();

      vi.useFakeTimers();
      act(() => {
        // Wheel/touch/key intent can release the search pin before the pane's
        // follow-up scroll write arrives as a programmatic event.
        fireEvent.wheel(harness.scrollNode, { deltaY: -48 });
        harness.setScrollTop(0);
        notifyMessageStackScrollWrite(harness.scrollNode, {
          scrollKind: "page_jump",
        });
      });

      expect(screen.getByText("message-1")).toBeInTheDocument();

      await advanceIdleMountedRangeCompaction();
      expect(screen.queryByText("message-140")).not.toBeInTheDocument();
    } finally {
      harness.restore();
    }
  });

  it("uses the estimated search target when the mounted target has zero geometry", async () => {
    const messages = makeTextMessages(160);
    const matchedKeys = new Set(messages.map((message) => `message:${message.id}`));
    const harness = renderVirtualizedHarness({
      messages,
      conversationSearchQuery: "Message",
      conversationSearchMatchedItemKeys: matchedKeys,
      conversationSearchActiveItemKey: "message:message-140",
      slotRect: (message) =>
        message.id === "message-140" ? { height: 0, top: 0 } : null,
    });

    try {
      await waitFor(() => {
        expect(screen.getByText("message-140")).toBeInTheDocument();
        expect(
          harness.container.querySelector('[data-message-id="message-140"]'),
        ).toBeInTheDocument();
        expect(harness.scrollTop).toBeGreaterThanOrEqual(18_000);
        expect(harness.scrollTop).toBeLessThan(19_000);
      });
    } finally {
      harness.restore();
    }
  });

  it("keeps the virtualizer handle stable while a search result remains pinned", async () => {
    const messages = makeTextMessages(160);
    const matchedKeys = new Set(messages.map((message) => `message:${message.id}`));
    const virtualizerHandleRef: VirtualizedConversationMessageListHandleRef = {
      current: null,
    };
    let renderCount = 0;
    const harness = renderVirtualizedHarness({
      messages,
      conversationSearchQuery: "Message",
      conversationSearchMatchedItemKeys: matchedKeys,
      conversationSearchActiveItemKey: "message:message-140",
      renderMessageCard: (message) => {
        renderCount += 1;
        return <article className="message-card">{message.id}</article>;
      },
      virtualizerHandleRef,
    });

    try {
      await waitFor(() => {
        expect(screen.getByText("message-140")).toBeInTheDocument();
        expect(virtualizerHandleRef.current).not.toBeNull();
        expect(harness.scrollTop).toBeGreaterThanOrEqual(18_000);
        expect(harness.scrollTop).toBeLessThan(19_000);
      });
      const initialHandle = virtualizerHandleRef.current!;
      expect(initialHandle.jumpToMessageIndex(170)).toBe(false);
      const renderCountBeforeRerender = renderCount;

      act(() => {
        harness.rerenderWithSearch({
          conversationSearchQuery: "Message",
          conversationSearchMatchedItemKeys: matchedKeys,
          conversationSearchActiveItemKey: "message:message-140",
        });
      });

      await waitFor(() => {
        const renderDelta = renderCount - renderCountBeforeRerender;
        expect(renderDelta).toBeGreaterThanOrEqual(5);
        expect(renderDelta).toBeLessThan(120);
        expect(virtualizerHandleRef.current).toBe(initialHandle);
      });

      const expandedMessages = makeTextMessages(180);
      const expandedMatchedKeys = new Set(
        expandedMessages.map((message) => `message:${message.id}`),
      );
      act(() => {
        harness.rerenderWithMessages(expandedMessages, {
          conversationSearchQuery: "Message",
          conversationSearchMatchedItemKeys: expandedMatchedKeys,
          conversationSearchActiveItemKey: "message:message-140",
        });
      });

      await waitFor(() => {
        expect(virtualizerHandleRef.current).toBe(initialHandle);
        expect(initialHandle.getLayoutSnapshot().messageCount).toBe(180);
      });

      act(() => {
        expect(
          initialHandle.jumpToMessageIndex(170, {
            align: "center",
            flush: true,
          }),
        ).toBe(true);
      });

      await waitFor(() => {
        expect(screen.getByText("message-171")).toBeInTheDocument();
      });
    } finally {
      harness.restore();
    }
  });

  it("clears the virtualizer handle ref on unmount", async () => {
    const virtualizerHandleRef: VirtualizedConversationMessageListHandleRef = {
      current: null,
    };
    const harness = renderVirtualizedHarness({
      messages: makeTextMessages(48),
      virtualizerHandleRef,
    });

    try {
      await waitFor(() => {
        expect(virtualizerHandleRef.current).not.toBeNull();
      });

      harness.unmount();
      expect(virtualizerHandleRef.current).toBeNull();
    } finally {
      harness.restore();
    }
  });
});
