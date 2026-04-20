import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { RefObject } from "react";
import { describe, expect, it, vi } from "vitest";

import {
  AgentSessionPanel,
  AgentSessionPanelFooter,
} from "./AgentSessionPanel";
import { VirtualizedConversationMessageList } from "./VirtualizedConversationMessageList";
import { RunningIndicator } from "./session-activity-cards";
import {
  getAdjustedVirtualizedScrollTopForHeightChange,
  getScrollContainerBottomGap,
  isScrollContainerNearBottom,
} from "./conversation-virtualization";
import type { Message, Session } from "../types";

function makeSession(id: string, overrides?: Partial<Session>): Session {
  return {
    id,
    name: id,
    emoji: "x",
    agent: "Codex",
    workdir: "/tmp",
    model: "test-model",
    status: "idle",
    preview: "",
    messages: [],
    ...overrides,
  };
}

function makeTextMessages(count: number): Message[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `message-${index + 1}`,
    type: "text",
    timestamp: `10:${String(index).padStart(2, "0")}`,
    author: index % 2 === 0 ? "you" : "assistant",
    text: `Message ${index + 1}`,
  }));
}

function renderSessionPanelWithDefaults(
  props: Partial<Parameters<typeof AgentSessionPanel>[0]>,
) {
  const activeSession = props.activeSession ?? null;
  return render(
    <AgentSessionPanel
      paneId="pane-1"
      viewMode="session"
      activeSession={activeSession}
      isLoading={false}
      isUpdating={false}
      showWaitingIndicator={false}
      waitingIndicatorPrompt={null}
      mountedSessions={activeSession ? [activeSession] : []}
      commandMessages={[]}
      diffMessages={[]}
      scrollContainerRef={{ current: document.createElement("section") }}
      onApprovalDecision={() => {}}
      onUserInputSubmit={() => {}}
      onMcpElicitationSubmit={() => {}}
      onCodexAppRequestSubmit={() => {}}
      onCancelQueuedPrompt={() => {}}
      onSessionSettingsChange={() => {}}
      conversationSearchQuery=""
      conversationSearchMatchedItemKeys={new Set()}
      conversationSearchActiveItemKey={null}
      onConversationSearchItemMount={() => {}}
      renderCommandCard={() => null}
      renderDiffCard={() => null}
      renderMessageCard={(message) => (
        <article className="message-card">{message.id}</article>
      )}
      renderPromptSettings={() => null}
      {...props}
    />,
  );
}

describe("AgentSessionPanel conversation caching", () => {
  it("keeps the inactive virtualized container mounted without rendering stale cards", () => {
    const cachedSession = makeSession("cached-session", {
      messages: makeTextMessages(85),
    });
    const activeSession = makeSession("active-session", {
      messages: makeTextMessages(1),
    });

    const { container } = renderSessionPanelWithDefaults({
      activeSession,
      mountedSessions: [cachedSession, activeSession],
    });

    const hiddenCachedPage = container.querySelector(
      '.session-conversation-page[hidden]',
    );
    expect(hiddenCachedPage).not.toBeNull();
    const inactiveVirtualizedList = hiddenCachedPage?.querySelector(
      ".virtualized-message-list",
    );
    expect(inactiveVirtualizedList).not.toBeNull();
    expect(
      hiddenCachedPage?.querySelector(".message-card"),
    ).toBeNull();
  });

  it("keeps long session find virtualized while typing a query", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    const scrollNode = document.createElement("section");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => 500,
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      queueMicrotask(() => callback(0));
      return 1;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;

    try {
      const messages = makeTextMessages(180);
      const activeSession = makeSession("session-a", { messages });
      const matchedKeys = new Set(messages.map((message) => `message:${message.id}`));

      const { container } = renderSessionPanelWithDefaults({
        activeSession,
        mountedSessions: [activeSession],
        scrollContainerRef: {
          current: scrollNode,
        } as RefObject<HTMLElement | null>,
        conversationSearchQuery: "Message",
        conversationSearchMatchedItemKeys: matchedKeys,
        conversationSearchActiveItemKey: "message:message-150",
      });

      await waitFor(() => {
        expect(screen.getByText("message-150")).toBeInTheDocument();
      });

      expect(container.querySelectorAll(".message-card").length).toBeLessThan(80);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
    }
  });

  it("keeps the bottom pin across successive virtualized height commits", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    let measuredSlotHeight = 180;
    let scrollTop = 400;
    let scrollHeightQueue = [500];
    const scrollWrites: number[] = [];

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
      get: () => 100,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () =>
        scrollHeightQueue.length > 1
          ? scrollHeightQueue.shift()!
          : (scrollHeightQueue[0] ?? 500),
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
        scrollWrites.push(nextValue);
      },
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    let nextFrameId = 1;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      queueMicrotask(() => callback(0));
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      const height = element.classList.contains("virtualized-message-slot")
        ? measuredSlotHeight
        : 100;
      return {
        bottom: height,
        height,
        left: 0,
        right: 100,
        top: 0,
        width: 100,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      } as DOMRect;
    };

    try {
      const scrollContainerRef = {
        current: scrollNode,
      } as RefObject<HTMLElement | null>;
      const { container } = render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={makeTextMessages(3)}
          scrollContainerRef={scrollContainerRef}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      const slot = await waitFor(() => {
        const candidate = container.querySelector(".virtualized-message-slot");
        expect(candidate).not.toBeNull();
        return candidate!;
      });

      scrollWrites.length = 0;
      scrollTop = 400;
      measuredSlotHeight = 260;
      scrollHeightQueue = [500, 580, 580];
      await act(async () => {
        resizeCallbacks.get(slot)?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      expect(scrollWrites).toContain(480);

      const secondSlot = await waitFor(() => {
        const candidate = container.querySelector(".virtualized-message-slot");
        expect(candidate).not.toBeNull();
        expect(resizeCallbacks.has(candidate!)).toBe(true);
        return candidate!;
      });
      scrollWrites.length = 0;
      scrollTop = 480;
      measuredSlotHeight = 340;
      scrollHeightQueue = [580, 660, 660];
      await act(async () => {
        resizeCallbacks.get(secondSlot)?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      expect(scrollWrites).toContain(560);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("skips the bottom pin write while the user is actively scrolling", async () => {
    // Regression guard for the "near-bottom streaming re-pins the
    // viewport and fights user scroll-up" symptom. Inside
    // `USER_SCROLL_ADJUSTMENT_COOLDOWN_MS` (200 ms) of a `wheel` /
    // `touchmove` / `keydown` event on the scroll container, a
    // streaming-driven height measurement must not snap `scrollTop`
    // back to the bottom — even when the user is still within the
    // 72 px near-bottom band. After the cooldown expires, normal
    // pinning resumes.
    //
    // Two code paths must honour the cooldown: `handleHeightChange`'s
    // `shouldKeepBottom` branch (which would `node.scrollTop = target`
    // inline), AND the separate re-pin `useLayoutEffect` keyed on
    // `layout.totalHeight` (which fires on the follow-up commit from
    // `setLayoutVersion` and would otherwise re-pin a frame later).
    // The harness uses a live `scrollHeight` derived from
    // `measuredSlotHeight` so both paths see the post-measurement
    // geometry — reverting either gate reproduces a write we can
    // assert against.
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    let measuredSlotHeight = 180;
    let scrollTop = 400;
    const scrollWrites: number[] = [];

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
      get: () => 100,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      // Live `scrollHeight` derived from the current measured slot
      // height. Matches what real DOM exposes once React commits the
      // wrapper's `style={{ height: layout.totalHeight }}`: the
      // measurement updates `messageHeightsRef`, `setLayoutVersion`
      // fires, `layout.totalHeight` recomputes, the wrapper re-renders
      // with the new height, and subsequent `scrollHeight` reads
      // reflect the new geometry. Encoding that dependency directly
      // here stops the test from accidentally passing against a stale
      // pre-measurement value.
      get: () => 500 + (measuredSlotHeight - 180),
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
        scrollWrites.push(nextValue);
      },
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    let nextFrameId = 1;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      queueMicrotask(() => callback(0));
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      const height = element.classList.contains("virtualized-message-slot")
        ? measuredSlotHeight
        : 100;
      return {
        bottom: height,
        height,
        left: 0,
        right: 100,
        top: 0,
        width: 100,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      } as DOMRect;
    };

    try {
      const scrollContainerRef = {
        current: scrollNode,
      } as RefObject<HTMLElement | null>;
      const { container } = render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={makeTextMessages(3)}
          scrollContainerRef={scrollContainerRef}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      const slot = await waitFor(() => {
        const candidate = container.querySelector(".virtualized-message-slot");
        expect(candidate).not.toBeNull();
        return candidate!;
      });

      // Simulate the user wheeling up inside the scroll container. The
      // component's `syncViewport` effect attaches a `wheel` listener
      // on `scrollNode` that timestamps the last direct-scroll input.
      fireEvent.wheel(scrollNode, { deltaY: -50 });

      // Trigger a streaming-style height measurement. `scrollTop` is
      // intentionally at 400 (clientHeight 100 / pre-measurement
      // scrollHeight 500 → gap 0 → `isScrollContainerNearBottom` is
      // true, pin heuristic armed). The measurement grows the card
      // from 180 → 260 px; the live `scrollHeight` getter then
      // reports 580. Without the cooldown on EITHER the inline
      // `handleHeightChange` write path OR the follow-up re-pin
      // `useLayoutEffect`, `scrollTop` would be written to the new
      // pin target (580 − 100 = 480). The cooldown must suppress
      // both paths.
      scrollWrites.length = 0;
      scrollTop = 400;
      measuredSlotHeight = 260;
      await act(async () => {
        resizeCallbacks.get(slot)?.([] as unknown as ResizeObserverEntry[], {} as ResizeObserver);
        await Promise.resolve();
      });

      expect(scrollWrites).toEqual([]);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("keeps the auto-load scroll listener bound across many scroll events", async () => {
    // Regression guard for the re-subscribe churn in the
    // auto-load-older-messages effect. Before the fix, the effect
    // depended on `viewportVisibleRange.startIndex`, which changes on
    // every scroll event (via `syncViewport` -> `setViewport` ->
    // `viewportVisibleRange` memo re-run). Each dep change ran the
    // cleanup (`removeEventListener`) and re-ran the body
    // (`addEventListener`). Between those two steps a wheel event
    // could land with no listener bound. After the fix the effect
    // reads `viewportVisibleRange` through a ref, so the listener is
    // attached once per `isActive` / `hasOlderMessages` / `sessionId`
    // transition — and scroll events no longer churn it.
    const OriginalResizeObserver = window.ResizeObserver;
    const originalRequestAnimationFrame = window.requestAnimationFrame;
    const originalCancelAnimationFrame = window.cancelAnimationFrame;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();
    let scrollTop = 50;
    const addScrollCalls: Array<{ options?: AddEventListenerOptions | boolean }> = [];
    const removeScrollCalls: Array<Record<string, never>> = [];

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
      get: () => 500,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 2000,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (nextValue: number) => {
        scrollTop = nextValue;
      },
    });
    // Wrap addEventListener / removeEventListener to count scroll
    // registrations specifically. Other listeners (wheel, touchmove,
    // keydown) pass through unchanged.
    const originalAdd = scrollNode.addEventListener.bind(scrollNode);
    const originalRemove = scrollNode.removeEventListener.bind(scrollNode);
    scrollNode.addEventListener = ((
      event: string,
      handler: EventListenerOrEventListenerObject,
      options?: AddEventListenerOptions | boolean,
    ) => {
      if (event === "scroll") {
        addScrollCalls.push({ options });
      }
      return originalAdd(event, handler, options);
    }) as typeof scrollNode.addEventListener;
    scrollNode.removeEventListener = ((
      event: string,
      handler: EventListenerOrEventListenerObject,
      options?: AddEventListenerOptions | boolean,
    ) => {
      if (event === "scroll") {
        removeScrollCalls.push({});
      }
      return originalRemove(event, handler, options);
    }) as typeof scrollNode.removeEventListener;

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    let nextFrameId = 1;
    window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      queueMicrotask(() => callback(0));
      return frameId;
    }) as typeof requestAnimationFrame;
    window.cancelAnimationFrame = vi.fn() as unknown as typeof cancelAnimationFrame;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      const height = element.classList.contains("virtualized-message-slot")
        ? 180
        : 500;
      return {
        bottom: height,
        height,
        left: 0,
        right: 100,
        top: 0,
        width: 100,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      } as DOMRect;
    };

    try {
      const scrollContainerRef = {
        current: scrollNode,
      } as RefObject<HTMLElement | null>;
      // 210 messages -> windowStartIndex = 210 - 200 = 10 -> hasOlderMessages = true
      // triggers the auto-load scroll effect and attaches a listener.
      render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={makeTextMessages(210)}
          scrollContainerRef={scrollContainerRef}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      // Let post-activation measurements settle. The scroll listener
      // should be attached once (the syncViewport effect) plus once
      // more when the auto-load effect runs (so 2 total).
      await waitFor(() => {
        expect(addScrollCalls.length).toBeGreaterThanOrEqual(2);
      });
      const baselineAddCount = addScrollCalls.length;
      const baselineRemoveCount = removeScrollCalls.length;

      // Fire a burst of scroll events that each drive a
      // `setViewport` -> `viewportVisibleRange` recompute. Before
      // the fix, each event would churn the auto-load listener
      // (add + remove). After the fix, the counts stay flat.
      for (let i = 0; i < 10; i += 1) {
        scrollTop = 1000 - i * 25;
        await act(async () => {
          scrollNode.dispatchEvent(new Event("scroll"));
          await Promise.resolve();
        });
      }

      expect(addScrollCalls.length).toBe(baselineAddCount);
      expect(removeScrollCalls.length).toBe(baselineRemoveCount);
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("completes post-activation measuring when the first real height matches the estimate", async () => {
    const OriginalResizeObserver = window.ResizeObserver;
    const originalGetBoundingClientRect = Element.prototype.getBoundingClientRect;
    const resizeCallbacks = new Map<Element, ResizeObserverCallback>();

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
      get: () => 500,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 102,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => 0,
      set: () => {},
    });

    window.ResizeObserver = ResizeObserverMock as unknown as typeof ResizeObserver;
    Element.prototype.getBoundingClientRect = function getBoundingClientRectMock() {
      const element = this as HTMLElement;
      const height = element.classList.contains("virtualized-message-slot")
        ? 102
        : 500;
      return {
        bottom: height,
        height,
        left: 0,
        right: 100,
        top: 0,
        width: 100,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      } as DOMRect;
    };

    try {
      const scrollContainerRef = {
        current: scrollNode,
      } as RefObject<HTMLElement | null>;
      const { container } = render(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={makeTextMessages(1)}
          scrollContainerRef={scrollContainerRef}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      const list = container.querySelector(".virtualized-message-list");
      expect(list).not.toBeNull();
      expect(list).not.toHaveClass("is-measuring-post-activation");
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });

  it("arms post-activation measuring synchronously on inactive -> active transitions (no visible pre-measure paint)", async () => {
    // Regression guard for the "shows messages, then empty" flicker on
    // tab switch. The previous `useLayoutEffect`-based transition left
    // a render frame where `isActive=true` was committed with
    // `measuring=false`, allowing the browser to paint message cards
    // at mis-estimated positions before the effect re-hid them for
    // re-measurement. After the fix, the transition is detected during
    // render so `measuring=true` commits in the SAME render as
    // `isActive=true`.
    const OriginalResizeObserver = window.ResizeObserver;
    const originalGetBoundingClientRect =
      Element.prototype.getBoundingClientRect;

    class ResizeObserverMock {
      observe() {}
      disconnect() {}
    }

    const scrollNode = document.createElement("div");
    Object.defineProperty(scrollNode, "clientHeight", {
      configurable: true,
      get: () => 500,
    });
    Object.defineProperty(scrollNode, "scrollHeight", {
      configurable: true,
      get: () => 500,
    });
    Object.defineProperty(scrollNode, "scrollTop", {
      configurable: true,
      get: () => 0,
      set: () => {},
    });

    window.ResizeObserver =
      ResizeObserverMock as unknown as typeof ResizeObserver;
    // Intentionally never resolve measurements: that keeps the list in
    // the measuring phase across the whole test, so the assertion below
    // pins "class present immediately after the transition" rather than
    // "class present at some later settled state".
    Element.prototype.getBoundingClientRect =
      function getBoundingClientRectMock() {
        return {
          bottom: 0,
          height: 0,
          left: 0,
          right: 0,
          top: 0,
          width: 0,
          x: 0,
          y: 0,
          toJSON: () => ({}),
        } as DOMRect;
      };

    try {
      const scrollContainerRef = {
        current: scrollNode,
      } as RefObject<HTMLElement | null>;

      // Mount inactive with messages already present, matching the
      // cached-but-not-focused tab shape.
      const { container, rerender } = render(
        <VirtualizedConversationMessageList
          isActive={false}
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={makeTextMessages(120)}
          scrollContainerRef={scrollContainerRef}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      // While inactive, no measuring.
      const preList = container.querySelector(".virtualized-message-list");
      expect(preList).not.toHaveClass("is-measuring-post-activation");

      // Flip to active — the "tab switch" scenario.
      rerender(
        <VirtualizedConversationMessageList
          isActive
          renderMessageCard={(message) => (
            <article className="message-card">{message.id}</article>
          )}
          sessionId="session-a"
          messages={makeTextMessages(120)}
          scrollContainerRef={scrollContainerRef}
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
          onMcpElicitationSubmit={() => {}}
          onCodexAppRequestSubmit={() => {}}
        />,
      );

      // The list must carry the measuring class IMMEDIATELY after the
      // rerender — no intermediate frame with isActive=true but
      // measuring=false. testing-library's `rerender` is synchronous;
      // if the transition were detected in a useLayoutEffect instead
      // of during render, React would still commit one DOM state
      // without the class before the effect set it. The querySelector
      // here reads the DOM after the transition commit completes.
      const postList = container.querySelector(".virtualized-message-list");
      expect(postList).not.toBeNull();
      expect(postList).toHaveClass("is-measuring-post-activation");
    } finally {
      window.ResizeObserver = OriginalResizeObserver;
      Element.prototype.getBoundingClientRect = originalGetBoundingClientRect;
    }
  });
});

function renderFooter({
  isPaneActive = true,
  session,
  committedDraft = "",
  isUpdating = false,
  onDraftCommit = vi.fn(),
  modelOptionsError = null,
  agentCommands = [],
  hasLoadedAgentCommands = true,
  isRefreshingAgentCommands = false,
  agentCommandsError = null,
  onRefreshSessionModelOptions = vi.fn(),
  onRefreshAgentCommands = vi.fn(),
  onSend = vi.fn(() => true),
  onSessionSettingsChange = vi.fn(),
}: {
  isPaneActive?: boolean;
  session: Session | null;
  committedDraft?: string;
  isUpdating?: boolean;
  onDraftCommit?: (sessionId: string, nextValue: string) => void;
  modelOptionsError?: string | null;
  agentCommands?: {
    kind?: "promptTemplate" | "nativeSlash";
    name: string;
    description: string;
    content: string;
    source: string;
    argumentHint?: string | null;
  }[];
  hasLoadedAgentCommands?: boolean;
  isRefreshingAgentCommands?: boolean;
  agentCommandsError?: string | null;
  onRefreshSessionModelOptions?: (sessionId: string) => void;
  onRefreshAgentCommands?: (sessionId: string) => void;
  onSend?: (sessionId: string, draftText?: string, expandedText?: string | null) => boolean;
  onSessionSettingsChange?: (sessionId: string, field: string, value: string) => void;
}) {
  return (
    <AgentSessionPanelFooter
      paneId="pane-1"
      viewMode="session"
      isPaneActive={isPaneActive}
      activeSession={session}
      committedDraft={committedDraft}
      draftAttachments={[]}
      formatByteSize={(byteSize) => `${byteSize} B`}
      isSending={false}
      isStopping={false}
      isSessionBusy={false}
      isUpdating={isUpdating}
      showNewResponseIndicator={false}
      footerModeLabel="Session"
      onScrollToLatest={() => {}}
      onDraftCommit={onDraftCommit}
      onDraftAttachmentRemove={() => {}}
      isRefreshingModelOptions={false}
      modelOptionsError={modelOptionsError}
      agentCommands={agentCommands}
      hasLoadedAgentCommands={hasLoadedAgentCommands}
      isRefreshingAgentCommands={isRefreshingAgentCommands}
      agentCommandsError={agentCommandsError}
      onRefreshSessionModelOptions={onRefreshSessionModelOptions}
      onRefreshAgentCommands={onRefreshAgentCommands}
      onSend={onSend}
      onSessionSettingsChange={onSessionSettingsChange}
      onStopSession={() => {}}
      onPaste={() => {}}
    />
  );
}

describe("getAdjustedVirtualizedScrollTopForHeightChange", () => {
  it("preserves the viewport anchor when a measured message above the viewport changes height", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 1200,
        messageTop: 900,
        nextHeight: 260,
        previousHeight: 180,
      }),
    ).toBe(1280);
  });
  it("does not jump the viewport when a partially visible message above the fold changes height", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 1200,
        messageTop: 1190,
        nextHeight: 200,
        previousHeight: 100,
      }),
    ).toBe(1200);
  });
  it("adjusts when a message is fully above the viewport with its bottom exactly at scrollTop", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 1200,
        messageTop: 1000,
        nextHeight: 260,
        previousHeight: 200,
      }),
    ).toBe(1260);
  });
  it("does not adjust when a message starts exactly at the viewport top", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 1200,
        messageTop: 1200,
        nextHeight: 260,
        previousHeight: 180,
      }),
    ).toBe(1200);
  });
  it("does not snap back when a newly visible message below the current viewport is measured", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 1200,
        messageTop: 1320,
        nextHeight: 260,
        previousHeight: 180,
      }),
    ).toBe(1200);
  });
  it("adjusts when a partially visible message above the fold shrinks", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 100,
        messageTop: 50,
        nextHeight: 60,
        previousHeight: 100,
      }),
    ).toBe(60);
  });
  it("floors negative height deltas at zero when the anchor would move above the top", () => {
    expect(
      getAdjustedVirtualizedScrollTopForHeightChange({
        currentScrollTop: 50,
        messageTop: 20,
        nextHeight: 100,
        previousHeight: 200,
      }),
    ).toBe(0);
  });
});

describe("getScrollContainerBottomGap", () => {
  // Small helper that accepts a plain object rather than a real DOM
  // element — the production signature is `Pick<HTMLElement,
  // "clientHeight" | "scrollHeight" | "scrollTop">` for exactly this
  // reason, so the boundary tests can pin behavior without touching
  // jsdom geometry.
  function node(
    scrollHeight: number,
    clientHeight: number,
    scrollTop: number,
  ) {
    return { scrollHeight, clientHeight, scrollTop };
  }

  it("returns the raw distance from scrollTop to the bottom when positive", () => {
    expect(getScrollContainerBottomGap(node(1000, 200, 0))).toBe(800);
    expect(getScrollContainerBottomGap(node(1000, 200, 100))).toBe(700);
    expect(getScrollContainerBottomGap(node(1000, 200, 799))).toBe(1);
  });

  it("floors negative gaps at zero when content is shorter than the viewport", () => {
    // Content shorter than viewport: `scrollHeight - clientHeight -
    // scrollTop` is negative. The helper must clamp to `0` so downstream
    // `<= N` / `< N` checks treat the viewport as "at bottom" without
    // special-casing the short-content layout.
    expect(getScrollContainerBottomGap(node(300, 400, 0))).toBe(0);
    expect(getScrollContainerBottomGap(node(200, 200, 0))).toBe(0);
  });
});

describe("isScrollContainerNearBottom", () => {
  function node(
    scrollHeight: number,
    clientHeight: number,
    scrollTop: number,
  ) {
    return { scrollHeight, clientHeight, scrollTop };
  }

  // The near-bottom threshold is `< 72 px`, intentionally chosen to
  // match the parent pane's sticky threshold in
  // `syncMessageStackScrollPosition`. A previous revision used
  // `<= 96 px` which created a 72-96 px "dead band" where the parent
  // had recorded `shouldStick: false` (the user scrolled up past the
  // 72 threshold) but a later virtualized measurement would still
  // re-pin the viewport to the latest message — the user felt
  // "snatched back" on code-block tokenization or image loads. These
  // boundary tests lock in the 72 value so anyone changing it in only
  // one file has to change it here too.
  it("returns true at gap 0 (exactly at bottom)", () => {
    expect(isScrollContainerNearBottom(node(1000, 200, 800))).toBe(true);
  });
  it("returns true at gap 71 (1 px inside the near-bottom boundary)", () => {
    expect(isScrollContainerNearBottom(node(1000, 200, 729))).toBe(true);
  });
  it("returns false at gap 72 (matches App.tsx strict-less-than sticky boundary)", () => {
    expect(isScrollContainerNearBottom(node(1000, 200, 728))).toBe(false);
  });
  it("returns false at gap 96 (proves the 96-px band is no longer near-bottom)", () => {
    expect(isScrollContainerNearBottom(node(1000, 200, 704))).toBe(false);
  });
  it("returns true for short content whose gap clamps to 0", () => {
    expect(isScrollContainerNearBottom(node(300, 400, 0))).toBe(true);
  });
});

describe("AgentSessionPanelFooter", () => {
  it("shows a command badge in the live turn card for slash commands", () => {
    render(<RunningIndicator agent="Codex" lastPrompt="/review-local" />);

    expect(screen.getAllByText("Command")).toHaveLength(2);
    expect(screen.getByText("Executing a command...")).toBeInTheDocument();
  });

  it("does not show a command badge in the live turn card for regular prompts", () => {
    render(<RunningIndicator agent="Codex" lastPrompt="Review the staged diff" />);

    expect(screen.queryByText("Command")).not.toBeInTheDocument();
    expect(screen.getByText("Waiting for the next chunk of output...")).toBeInTheDocument();
  });

  it("does not commit a draft during unrelated session rerenders", () => {
    const initialCommit = vi.fn();
    const nextCommit = vi.fn();
    const sessionId = "session-a";
    const { rerender } = render(
      renderFooter({
        onDraftCommit: initialCommit,
        session: makeSession(sessionId, { preview: "first preview" }),
      }),
    );

    const textarea = screen.getByLabelText(`Message ${sessionId}`);
    fireEvent.change(textarea, { target: { value: "draft in progress" } });

    rerender(
      renderFooter({
        onDraftCommit: nextCommit,
        session: makeSession(sessionId, { preview: "streamed preview", status: "active" }),
      }),
    );

    expect(initialCommit).not.toHaveBeenCalled();
    expect(nextCommit).not.toHaveBeenCalled();
    expect(screen.getByLabelText(`Message ${sessionId}`)).toHaveValue("draft in progress");
  });

  it("commits the in-progress draft when switching sessions", () => {
    const onDraftCommit = vi.fn();
    const { rerender } = render(
      renderFooter({
        onDraftCommit,
        session: makeSession("session-a"),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "carry this draft" },
    });

    rerender(
      renderFooter({
        onDraftCommit,
        session: makeSession("session-b"),
      }),
    );

    expect(onDraftCommit).toHaveBeenCalledWith("session-a", "carry this draft");
  });

  it("focuses the prompt when a session opens in the active pane", async () => {
    const { rerender } = render(
      renderFooter({
        session: null,
      }),
    );

    rerender(
      renderFooter({
        session: makeSession("session-a"),
      }),
    );

    await waitFor(() => {
      expect(screen.getByLabelText("Message session-a")).toHaveFocus();
    });
  });

  it("shows the paste-only image attachment hint for active sessions", () => {
    render(
      renderFooter({
        session: makeSession("session-a"),
      }),
    );

    expect(
      screen.getByText(
        "Paste PNG, JPEG, GIF, or WebP images into the prompt. Drag-and-drop is not supported yet.",
      ),
    ).toBeInTheDocument();
  });

  it("does not focus the prompt for an inactive pane", async () => {
    render(
      <>
        <button type="button">Outside focus</button>
        {renderFooter({
          isPaneActive: false,
          session: makeSession("session-a"),
        })}
      </>,
    );

    const outsideButton = screen.getByRole("button", { name: "Outside focus" });
    outsideButton.focus();
    expect(outsideButton).toHaveFocus();

    await waitFor(() => {
      expect(outsideButton).toHaveFocus();
    });
  });

  it("expands /model from the slash command menu", () => {
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/m" } });

    expect(screen.getByText("/model")).toBeInTheDocument();
    expect(screen.getByText("/mode")).toBeInTheDocument();

    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(screen.getByLabelText("Message session-a")).toHaveValue("/model ");
  });

  it("expands /effort from the Claude slash command menu", () => {
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/ef" } });

    expect(screen.getByText("/effort")).toBeInTheDocument();

    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(screen.getByLabelText("Message session-a")).toHaveValue("/effort ");
  });

  it("requests Claude agent commands when slash menu opens", async () => {
    const onRefreshAgentCommands = vi.fn();

    render(
      renderFooter({
        onRefreshAgentCommands,
        hasLoadedAgentCommands: false,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/" },
    });

    await waitFor(() => {
      expect(onRefreshAgentCommands).toHaveBeenCalledWith("session-a");
    });
  });

  it("re-requests Claude agent commands when the command revision changes", async () => {
    const onRefreshAgentCommands = vi.fn();
    const { rerender } = render(
      renderFooter({
        onRefreshAgentCommands,
        hasLoadedAgentCommands: true,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
          agentCommandsRevision: 0,
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "review-local",
            description: "Review local changes.",
            content: "Review local changes.",
            source: ".claude/commands/review-local.md",
          },
        ],
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/" },
    });
    expect(onRefreshAgentCommands).not.toHaveBeenCalled();

    rerender(
      renderFooter({
        onRefreshAgentCommands,
        hasLoadedAgentCommands: true,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
          agentCommandsRevision: 1,
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "review-local",
            description: "Review local changes.",
            content: "Review local changes.",
            source: ".claude/commands/review-local.md",
          },
        ],
      }),
    );

    await waitFor(() => {
      expect(onRefreshAgentCommands).toHaveBeenCalledWith("session-a");
    });
  });

  it("requests project agent commands for Codex when slash menu opens", async () => {
    const onRefreshAgentCommands = vi.fn();

    render(
      renderFooter({
        onRefreshAgentCommands,
        hasLoadedAgentCommands: false,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5",
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/" },
    });

    await waitFor(() => {
      expect(onRefreshAgentCommands).toHaveBeenCalledWith("session-a");
    });
  });

  it("shows agent commands alongside session controls", () => {
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
        agentCommands: [
          {
            name: "review-local",
            description: "Review staged and unstaged changes.",
            content: "Review staged and unstaged changes.",
            source: ".claude/commands/review-local.md",
          },
        ],
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/" },
    });

    expect(screen.getByText("Agent Commands")).toBeInTheDocument();
    expect(screen.getByText("Session Controls")).toBeInTheDocument();
    expect(screen.getByText("/review-local")).toBeInTheDocument();
    expect(screen.getByText("/model")).toBeInTheDocument();
  });

  it("sends a no-argument agent command directly from the slash menu", () => {
    const onSend = vi.fn(() => true);

    render(
      renderFooter({
        onSend,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
        agentCommands: [
          {
            kind: "nativeSlash",
            name: "review-local",
            description: "Review staged and unstaged changes.",
            content: "/review-local",
            source: "Claude project command",
          },
        ],
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSend).toHaveBeenCalledWith("session-a", "/review-local");
    expect(textarea).toHaveValue("");
  });

  it("expands an agent command with $ARGUMENTS and sends the substituted prompt", () => {
    const onSend = vi.fn(() => true);

    render(
      renderFooter({
        onSend,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
        agentCommands: [
          {
            kind: "promptTemplate",
            name: "fix-bug",
            description: "Fix a bug from docs/bugs.md by number.",
            content: `Fix the requested bug:

$ARGUMENTS

Verify the fix.`,
            source: ".claude/commands/fix-bug.md",
          },
        ],
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/fix" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(textarea).toHaveValue("/fix-bug ");

    fireEvent.change(textarea, { target: { value: "/fix-bug 3" } });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSend).toHaveBeenCalledWith(
      "session-a",
      "/fix-bug 3",
      `Fix the requested bug:

3

Verify the fix.`,
    );
    expect(textarea).toHaveValue("");
  });

  it("expands a native Claude command with arguments and sends the slash prompt", () => {
    const onSend = vi.fn(() => true);

    render(
      renderFooter({
        onSend,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
        }),
        agentCommands: [
          {
            kind: "nativeSlash",
            name: "review",
            description: "Review the current changes.",
            content: "/review",
            source: "Claude bundled command",
            argumentHint: "[scope]",
          },
        ],
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/rev" } });
    fireEvent.keyDown(textarea, { key: "Enter" });
    expect(textarea).toHaveValue("/review ");
    expect(onSend).not.toHaveBeenCalled();

    fireEvent.change(textarea, { target: { value: "/review staged files" } });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSend).toHaveBeenCalledWith("session-a", "/review staged files");
    expect(textarea).toHaveValue("");
  });

  it("applies a model slash command with keyboard navigation instead of sending a prompt", () => {
    const onSend = vi.fn(() => true);
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSend,
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5.4",
          modelOptions: [
            { label: "gpt-5.4", value: "gpt-5.4" },
            { label: "gpt-5.3-codex", value: "gpt-5.3-codex" },
          ],
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/model" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith("session-a", "model", "gpt-5.3-codex");
    expect(onSend).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Message session-a")).toHaveValue("");
  });

  it("applies a model slash choice on space without closing the slash menu", () => {
    const onSend = vi.fn(() => true);
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSend,
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5.4",
          modelOptions: [
            { label: "gpt-5.4", value: "gpt-5.4" },
            { label: "gpt-5.3-codex", value: "gpt-5.3-codex" },
          ],
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/model" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Space", code: "Space" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith("session-a", "model", "gpt-5.3-codex");
    expect(onSend).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Message session-a")).toHaveValue("/model");
    expect(screen.getByRole("listbox", { name: "Codex models" })).toBeInTheDocument();
  });

  it("applies a manual /model value when the live list does not include it", () => {
    const onSend = vi.fn(() => true);
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSend,
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5.4",
          modelOptions: [{ label: "gpt-5.4", value: "gpt-5.4" }],
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/model gpt-5.5-preview" } });
    expect(
      screen.getByText('Use "gpt-5.5-preview"'),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "gpt-5.5-preview is not in the current live model list. TermAl will still try it on the next prompt.",
      ),
    ).toBeInTheDocument();
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "model",
      "gpt-5.5-preview",
    );
    expect(onSend).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Message session-a")).toHaveValue("");
  });

  it("shows rich model metadata in the /model slash menu", () => {
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5.4",
          modelOptions: [
            {
              label: "GPT-5.4",
              value: "gpt-5.4",
              description: "Latest frontier agentic coding model.",
              badges: ["Recommended"],
              defaultReasoningEffort: "medium",
              supportedReasoningEfforts: ["low", "medium", "high"],
            },
          ],
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/model" },
    });

    expect(
      screen.getByText((content) =>
        content.includes("Latest frontier agentic coding model.") &&
        content.includes("Recommended") &&
        content.includes("Reasoning low, medium, high | Default medium"),
      ),
    ).toBeInTheDocument();
  });

  it("requests live Claude model options from /model when they have not loaded yet", async () => {
    const onRefreshSessionModelOptions = vi.fn();

    render(
      renderFooter({
        onRefreshSessionModelOptions,
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",

        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/model" } });

    await waitFor(() => {
      expect(onRefreshSessionModelOptions).toHaveBeenCalledWith("session-a");
    });
  });

  it("keeps the current /model option selected until the pointer moves", () => {
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Claude",
          model: "sonnet",
          modelOptions: [
            { label: "Sonnet", value: "sonnet" },
            { label: "Opus", value: "opus" },
          ],
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/model" },
    });

    const currentOption = screen.getByRole("option", { name: /Sonnet/i });
    const otherOption = screen.getByRole("option", { name: /Opus/i });
    expect(currentOption).toHaveAttribute("aria-selected", "true");
    expect(otherOption).toHaveAttribute("aria-selected", "false");

    fireEvent.mouseEnter(otherOption);
    expect(currentOption).toHaveAttribute("aria-selected", "true");
    expect(otherOption).toHaveAttribute("aria-selected", "false");

    fireEvent.mouseMove(otherOption);
    expect(currentOption).toHaveAttribute("aria-selected", "false");
    expect(otherOption).toHaveAttribute("aria-selected", "true");
  });

  it("applies Claude mode changes from /mode", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Claude",
          claudeApprovalMode: "ask",
          model: "sonnet",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/mode" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "claudeApprovalMode",
      "plan",
    );
  });

  it("applies Claude effort changes from /effort", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Claude",
          claudeEffort: "default",
          model: "sonnet",
          modelOptions: [
            {
              label: "Sonnet",
              value: "sonnet",
              badges: ["Effort"],
              supportedClaudeEffortLevels: ["low", "medium", "high"],
            },
          ],
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/effort" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "claudeEffort",
      "low",
    );
  });

  it("applies Codex approval and sandbox slash commands", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Codex",
          approvalPolicy: "never",
          sandboxMode: "workspace-write",
          model: "gpt-5",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/approvals" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "approvalPolicy",
      "on-request",
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/sandbox" },
    });
    fireEvent.keyDown(screen.getByLabelText("Message session-a"), { key: "ArrowDown" });
    fireEvent.keyDown(screen.getByLabelText("Message session-a"), { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "sandboxMode",
      "read-only",
    );
  });

  it("applies Codex reasoning effort changes from /effort", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/effort" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "reasoningEffort",
      "high",
    );
  });

  it("applies /effort on space without closing the slash menu", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/effort" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Space", code: "Space" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "reasoningEffort",
      "high",
    );
    expect(screen.getByLabelText("Message session-a")).toHaveValue("/effort");
    expect(screen.getByRole("listbox", { name: "Codex reasoning effort" })).toBeInTheDocument();
  });

  it("shows a pending slash state while session settings are applying", () => {
    render(
      renderFooter({
        isUpdating: true,
        committedDraft: "/effort",
        session: makeSession("session-a", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "high",
          sandboxMode: "workspace-write",
          model: "gpt-5.4",
        }),
      }),
    );

    expect(screen.getByText("Applying setting...")).toBeInTheDocument();
    expect(screen.getByText("Applying")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
  });

  it("resets the slash selection to the current model after the session model changes", async () => {
    const sessionA = makeSession("session-a", {
      agent: "Codex",
      model: "gpt-5.4",
      modelOptions: [
        { label: "gpt-5.4", value: "gpt-5.4" },
        { label: "gpt-5.3-codex", value: "gpt-5.3-codex" },
      ],
    });
    const { rerender } = render(
      renderFooter({
        session: sessionA,
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/model" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });

    expect(screen.getByRole("option", { name: /gpt-5\.3-codex/i })).toHaveAttribute(
      "aria-selected",
      "true",
    );

    rerender(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5.3-codex",
          modelOptions: [
            { label: "gpt-5.4", value: "gpt-5.4" },
            { label: "gpt-5.3-codex", value: "gpt-5.3-codex" },
          ],
        }),
      }),
    );

    await waitFor(() => {
      expect(screen.getByRole("option", { name: /gpt-5\.3-codex/i })).toHaveAttribute(
        "aria-selected",
        "true",
      );
      expect(screen.getByRole("option", { name: /gpt-5\.4/i })).toHaveAttribute(
        "aria-selected",
        "false",
      );
    });
  });

  it("limits /effort choices to the selected Codex model capabilities", () => {
    render(
      renderFooter({
        session: makeSession("session-a", {
          agent: "Codex",
          approvalPolicy: "never",
          reasoningEffort: "medium",
          sandboxMode: "workspace-write",
          model: "gpt-5-codex-mini",
          modelOptions: [
            {
              label: "GPT-5 Codex Mini",
              value: "gpt-5-codex-mini",
              description: "Optimized for codex. Cheaper, faster, but less capable.",
              defaultReasoningEffort: "medium",
              supportedReasoningEfforts: ["medium", "high"],
            },
          ],
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/effort" },
    });

    expect(screen.getByText(/GPT-5 Codex Mini supports medium, high\./)).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /medium/i })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /high/i })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /minimal/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /^low/i })).not.toBeInTheDocument();
  });

  it("applies Gemini mode changes from /mode", () => {
    const onSessionSettingsChange = vi.fn();

    render(
      renderFooter({
        onSessionSettingsChange,
        session: makeSession("session-a", {
          agent: "Gemini",
          geminiApprovalMode: "default",
          model: "auto",
        }),
      }),
    );

    const textarea = screen.getByLabelText("Message session-a");
    fireEvent.change(textarea, { target: { value: "/mode" } });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "ArrowDown" });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSessionSettingsChange).toHaveBeenCalledWith(
      "session-a",
      "geminiApprovalMode",
      "yolo",
    );
  });

  it("requests live Codex model options when /model opens", async () => {
    const onRefreshSessionModelOptions = vi.fn();

    render(
      renderFooter({
        onRefreshSessionModelOptions,
        session: makeSession("session-a", {
          agent: "Codex",
          model: "gpt-5.4",
          modelOptions: undefined,
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/model" },
    });

    await waitFor(() => {
      expect(onRefreshSessionModelOptions).toHaveBeenCalledWith("session-a");
    });
  });

  it("requests live Cursor model options when /model opens", async () => {
    const onRefreshSessionModelOptions = vi.fn();

    render(
      renderFooter({
        onRefreshSessionModelOptions,
        session: makeSession("session-a", {
          agent: "Cursor",
          cursorMode: "agent",
          model: "auto",
          modelOptions: undefined,
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/model" },
    });

    await waitFor(() => {
      expect(onRefreshSessionModelOptions).toHaveBeenCalledWith("session-a");
    });
  });

  it("shows inline model refresh errors and retries from the slash menu", () => {
    const onRefreshSessionModelOptions = vi.fn();

    render(
      renderFooter({
        modelOptionsError: "Cursor auth is not configured.",
        onRefreshSessionModelOptions,
        session: makeSession("session-a", {
          agent: "Cursor",
          cursorMode: "agent",
          model: "auto",
        }),
      }),
    );

    fireEvent.change(screen.getByLabelText("Message session-a"), {
      target: { value: "/model" },
    });

    expect(screen.getByRole("alert")).toHaveTextContent("Cursor auth is not configured.");

    fireEvent.click(screen.getByRole("button", { name: "Retry live models" }));

    expect(onRefreshSessionModelOptions).toHaveBeenCalledWith("session-a");
  });
});
