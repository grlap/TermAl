// Focused tests for active transcript page demand and undeferred live tails.

import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { RefObject } from "react";
import {
  MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
  notifyMessageStackUserScrollIntent,
} from "../message-stack-scroll-sync";

import {
  includeUndeferredMessageTail,
  useInitialActiveTranscriptMessages,
} from "./useInitialActiveTranscriptMessages";
import {
  addSessionHistoryPageDemandListener,
  completeSessionHistoryPageDemand,
  type SessionHistoryPageDemand,
} from "../session-history-demand";
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

function makeScrollNodeRef() {
  const node = document.createElement("div");
  document.body.append(node);
  return {
    cleanup: () => node.remove(),
    node,
    ref: { current: node } as RefObject<HTMLElement | null>,
  };
}

function renderTranscriptDemandHook({
  hasNewerHistory,
  hasOlderHistory,
  isActive = true,
  messageCount = 600,
  messages = makeTextMessages(20),
  messagesLoaded = false,
  scrollContainerRef = {
    current: document.createElement("div"),
  } as RefObject<HTMLElement | null>,
  sessionId = "session-a",
}: {
  hasNewerHistory?: boolean;
  hasOlderHistory?: boolean;
  isActive?: boolean;
  messageCount?: number | null;
  messages?: Message[];
  messagesLoaded?: boolean | null;
  scrollContainerRef?: RefObject<HTMLElement | null>;
  sessionId?: string;
} = {}) {
  return renderHook((props) => useInitialActiveTranscriptMessages(props), {
    initialProps: {
      hasNewerHistory,
      hasOlderHistory,
      isActive,
      messageCount,
      messages,
      messagesLoaded,
      scrollContainerRef,
      sessionId,
    },
  });
}

describe("includeUndeferredMessageTail", () => {
  it("uses current same-id updates at the active transcript tail", () => {
    const stable = makeTextMessages(1)[0]!;
    const deferred: Message = {
      author: "assistant",
      id: "message-2",
      text: "Old streamed answer",
      timestamp: "10:01",
      type: "text",
    };
    const current = { ...deferred, text: "Latest streamed answer" };

    expect(
      includeUndeferredMessageTail(
        [stable, deferred],
        [stable, current],
      ),
    ).toEqual([stable, current]);
  });

  it("drops deferred objects when the active session changes or clears", () => {
    const deferred = makeTextMessages(1);
    const current: Message[] = [
      {
        author: "assistant",
        id: "other-session",
        text: "Other",
        timestamp: "10:00",
        type: "text",
      },
    ];

    expect(includeUndeferredMessageTail(deferred, current)).toBe(current);
    expect(includeUndeferredMessageTail(deferred, [])).toEqual([]);
  });
});

describe("useInitialActiveTranscriptMessages", () => {
  it("keeps the supplied page unchanged and requests exactly one older page", () => {
    const messages = makeTextMessages(20);
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);
    const hook = renderTranscriptDemandHook({ messages });

    expect(hook.result.current.messages).toBe(messages);
    expect(hook.result.current.hasOlderHistory).toBe(true);
    act(() => {
      expect(hook.result.current.requestOlderTranscriptPage()).toBe(true);
    });
    expect(listener).toHaveBeenCalledWith({
      sessionId: "session-a",
      direction: "older",
    });

    removeListener();
  });

  it("does not request history when the supplied transcript is complete", () => {
    const hook = renderTranscriptDemandHook({
      messageCount: 3,
      messages: makeTextMessages(3),
      messagesLoaded: true,
    });

    expect(hook.result.current.hasOlderHistory).toBe(false);
    expect(hook.result.current.requestOlderTranscriptPage()).toBe(false);
  });

  it("requests another page after each completed prepend without writing scrollTop", () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    node.scrollTop = 32;
    const scrollWrites: number[] = [];
    const scrollTopDescriptor = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype,
      "scrollTop",
    );
    let scrollTop = 32;
    Object.defineProperty(node, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (value: number) => {
        scrollWrites.push(value);
        scrollTop = value;
      },
    });
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);
    const hook = renderTranscriptDemandHook({ scrollContainerRef: ref });

    act(() => {
      hook.result.current.requestOlderTranscriptPage();
    });
    hook.rerender({
      hasNewerHistory: undefined,
      hasOlderHistory: undefined,
      isActive: true,
      messageCount: 600,
      messages: makeTextMessages(84),
      messagesLoaded: false,
      scrollContainerRef: ref,
      sessionId: "session-a",
    });
    act(() => {
      hook.result.current.requestOlderTranscriptPage();
    });

    expect(listener).toHaveBeenCalledTimes(2);
    expect(scrollWrites).toEqual([]);

    removeListener();
    if (scrollTopDescriptor) {
      Object.defineProperty(node, "scrollTop", scrollTopDescriptor);
    }
    cleanup();
  });

  it("consumes upward wheel demand before a programmatic near-top scroll", async () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);
    renderTranscriptDemandHook({ scrollContainerRef: ref });

    await act(async () => {
      node.scrollTop = 0;
      node.dispatchEvent(
        new WheelEvent("wheel", { bubbles: true, deltaY: -20 }),
      );
      await Promise.resolve();
    });

    expect(listener).toHaveBeenCalledWith({
      sessionId: "session-a",
      direction: "older",
    });
    expect(listener).toHaveBeenCalledTimes(1);

    act(() => {
      node.dispatchEvent(new Event("scroll", { bubbles: true }));
    });
    expect(listener).toHaveBeenCalledTimes(1);

    await act(async () => {
      node.dispatchEvent(
        new WheelEvent("wheel", { bubbles: true, deltaY: -20 }),
      );
      await Promise.resolve();
    });
    expect(listener).toHaveBeenCalledTimes(2);

    removeListener();
    cleanup();
  });

  it("consumes body-owned keyboard demand when it reaches the loaded top", () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);
    renderTranscriptDemandHook({ scrollContainerRef: ref });
    node.scrollTop = 400;

    act(() => {
      notifyMessageStackUserScrollIntent(node, {
        direction: "up",
        scrollKind: "incremental",
        viewportCanMove: false,
      });
      node.scrollTop = 0;
      node.dispatchEvent(new Event("scroll", { bubbles: true }));
    });

    expect(listener).toHaveBeenCalledWith({
      sessionId: "session-a",
      direction: "older",
    });
    expect(listener).toHaveBeenCalledTimes(1);
    removeListener();
    cleanup();
  });

  it("requests exactly one older page from the normalized owner", async () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    node.scrollTop = 0;
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);
    renderTranscriptDemandHook({ scrollContainerRef: ref });
    const publishNormalizedIntent = (event: KeyboardEvent) => {
      notifyMessageStackUserScrollIntent(node, {
        direction: "up",
        scrollKind: "incremental",
        sourceKeyboardEvent: event,
        viewportCanMove: false,
      });
    };
    node.addEventListener("keydown", publishNormalizedIntent);

    await act(async () => {
      node.dispatchEvent(
        new KeyboardEvent("keydown", { bubbles: true, key: "ArrowUp" }),
      );
      await Promise.resolve();
    });

    expect(listener).toHaveBeenCalledTimes(1);
    node.removeEventListener("keydown", publishNormalizedIntent);
    removeListener();
    cleanup();
  });

  it("requests exactly one older page for a body-targeted normalized key", async () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    node.scrollTop = 0;
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);
    renderTranscriptDemandHook({ scrollContainerRef: ref });
    const publishBodyOwnedIntent = (event: KeyboardEvent) => {
      if (event.target !== document.body || event.key !== "ArrowUp") {
        return;
      }
      notifyMessageStackUserScrollIntent(node, {
        detachFromBottomAtBoundary: true,
        direction: "up",
        scrollKind: "incremental",
        sourceKeyboardEvent: event,
        viewportCanMove: false,
      });
    };
    document.addEventListener("keydown", publishBodyOwnedIntent);

    try {
      await act(async () => {
        document.body.dispatchEvent(
          new KeyboardEvent("keydown", { bubbles: true, key: "ArrowUp" }),
        );
        await Promise.resolve();
      });

      expect(listener).toHaveBeenCalledTimes(1);
    } finally {
      document.removeEventListener("keydown", publishBodyOwnedIntent);
      removeListener();
      cleanup();
    }
  });

  it("does not turn bounded start or tail intent into ordinary pagination", () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    Object.defineProperties(node, {
      clientHeight: { configurable: true, value: 500 },
      scrollHeight: { configurable: true, value: 1_000 },
    });
    node.scrollTop = 0;
    const demands: SessionHistoryPageDemand[] = [];
    const removeListener = addSessionHistoryPageDemandListener((demand) => {
      demands.push(demand);
    });
    renderTranscriptDemandHook({
      hasNewerHistory: true,
      hasOlderHistory: true,
      scrollContainerRef: ref,
    });
    const homeEvent = new KeyboardEvent("keydown", {
      bubbles: true,
      key: "Home",
    });
    const endEvent = new KeyboardEvent("keydown", {
      bubbles: true,
      key: "End",
    });

    act(() => {
      notifyMessageStackUserScrollIntent(node, {
        direction: "up",
        scrollKind: "page_jump",
        sourceKeyboardEvent: homeEvent,
        viewportCanMove: false,
      });
      node.scrollTop = 500;
      notifyMessageStackUserScrollIntent(node, {
        direction: "down",
        scrollKind: "page_jump",
        sourceKeyboardEvent: endEvent,
        viewportCanMove: false,
      });
    });

    expect(demands).toEqual([]);
    removeListener();
    cleanup();
  });

  it("does not hydrate history for selection or unowned modifier keys", async () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    Object.defineProperties(node, {
      clientHeight: { configurable: true, value: 500 },
      scrollHeight: { configurable: true, value: 1_000 },
    });
    const demands: SessionHistoryPageDemand[] = [];
    const removeListener = addSessionHistoryPageDemandListener((demand) => {
      demands.push(demand);
      completeSessionHistoryPageDemand(demand.requestId, true);
    });
    renderTranscriptDemandHook({
      hasNewerHistory: true,
      hasOlderHistory: true,
      scrollContainerRef: ref,
    });

    await act(async () => {
      node.scrollTop = 0;
      node.dispatchEvent(
        new KeyboardEvent("keydown", {
          bubbles: true,
          key: "Home",
          shiftKey: true,
        }),
      );
      node.scrollTop = 500;
      node.dispatchEvent(
        new KeyboardEvent("keydown", {
          bubbles: true,
          key: "End",
          shiftKey: true,
        }),
      );
      node.scrollTop = 0;
      node.dispatchEvent(
        new KeyboardEvent("keydown", {
          bubbles: true,
          key: "ArrowUp",
          metaKey: true,
        }),
      );
      node.scrollTop = 500;
      node.dispatchEvent(
        new KeyboardEvent("keydown", {
          bubbles: true,
          key: "ArrowDown",
          metaKey: true,
        }),
      );
      await Promise.resolve();
    });

    expect(demands).toEqual([]);
    removeListener();
    cleanup();
  });

  it("consumes body-owned downward demand at the loaded bottom", async () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    Object.defineProperties(node, {
      clientHeight: { configurable: true, value: 500 },
      scrollHeight: { configurable: true, value: 1_000 },
    });
    node.scrollTop = 500;
    const demands: SessionHistoryPageDemand[] = [];
    const removeListener = addSessionHistoryPageDemandListener((demand) => {
      demands.push(demand);
      completeSessionHistoryPageDemand(demand.requestId, true);
    });
    renderTranscriptDemandHook({
      hasNewerHistory: true,
      hasOlderHistory: false,
      scrollContainerRef: ref,
    });

    await act(async () => {
      notifyMessageStackUserScrollIntent(node, {
        direction: "down",
        scrollKind: "page_jump",
        viewportCanMove: false,
      });
      await Promise.resolve();
    });

    expect(demands).toHaveLength(1);
    expect(demands[0]).toMatchObject({
      sessionId: "session-a",
      direction: "newer",
    });
    removeListener();
    cleanup();
  });

  it("defers history demand until every synchronous authority listener runs", async () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    node.scrollTop = 0;
    const observed: string[] = [];
    const removeDemandListener = addSessionHistoryPageDemandListener(() => {
      observed.push("history-demand");
    });
    renderTranscriptDemandHook({ scrollContainerRef: ref });
    const observeAuthority = () => observed.push("scroll-authority");
    // Register after the hook so this test stays valid even when a layout-effect
    // listener is later removed and re-added behind the history listener.
    node.addEventListener(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      observeAuthority,
    );

    await act(async () => {
      notifyMessageStackUserScrollIntent(node, {
        direction: "up",
        scrollKind: "incremental",
        viewportCanMove: false,
      });
      expect(observed).toEqual(["scroll-authority"]);
      await Promise.resolve();
    });

    expect(observed).toEqual(["scroll-authority", "history-demand"]);
    node.removeEventListener(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      observeAuthority,
    );
    removeDemandListener();
    cleanup();
  });

  it("ignores ordinary typing keys but handles nested-editor PageUp", async () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    const textarea = document.createElement("textarea");
    node.append(textarea);
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);
    renderTranscriptDemandHook({ scrollContainerRef: ref });

    await act(async () => {
      textarea.dispatchEvent(
        new KeyboardEvent("keydown", { bubbles: true, key: "ArrowUp" }),
      );
      await Promise.resolve();
    });
    expect(listener).not.toHaveBeenCalled();

    await act(async () => {
      textarea.dispatchEvent(
        new KeyboardEvent("keydown", { bubbles: true, key: "PageUp" }),
      );
      await Promise.resolve();
    });
    expect(listener).toHaveBeenCalledTimes(1);

    removeListener();
    cleanup();
  });

  it("does not paginate when the conversation overview slider owns navigation keys", async () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    const overviewSlider = document.createElement("div");
    overviewSlider.setAttribute("role", "slider");
    overviewSlider.dataset.testid = "conversation-overview-rail";
    node.append(overviewSlider);
    Object.defineProperties(node, {
      clientHeight: { configurable: true, value: 500 },
      scrollHeight: { configurable: true, value: 1_000 },
    });
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);
    renderTranscriptDemandHook({
      hasNewerHistory: true,
      hasOlderHistory: true,
      scrollContainerRef: ref,
    });

    try {
      node.scrollTop = 0;
      await act(async () => {
        for (const key of ["ArrowUp", "PageUp"]) {
          overviewSlider.dispatchEvent(
            new KeyboardEvent("keydown", { bubbles: true, key }),
          );
        }
        await Promise.resolve();
      });

      node.scrollTop = 500;
      await act(async () => {
        for (const key of ["ArrowDown", "PageDown"]) {
          overviewSlider.dispatchEvent(
            new KeyboardEvent("keydown", { bubbles: true, key }),
          );
        }
        await Promise.resolve();
      });

      expect(listener).not.toHaveBeenCalled();
    } finally {
      removeListener();
      cleanup();
    }
  });

  it("handles PageUp from a composer beside the transcript in the same workspace pane", () => {
    const pane = document.createElement("section");
    pane.className = "workspace-pane active";
    const node = document.createElement("div");
    const composer = document.createElement("textarea");
    pane.append(node, composer);
    document.body.append(pane);
    const ref = { current: node } as RefObject<HTMLElement | null>;
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);
    renderTranscriptDemandHook({ scrollContainerRef: ref });

    act(() => {
      composer.dispatchEvent(
        new KeyboardEvent("keydown", { bubbles: true, key: "PageUp" }),
      );
    });

    expect(listener).toHaveBeenCalledWith({
      sessionId: "session-a",
      direction: "older",
    });
    removeListener();
    pane.remove();
  });

  it("requests newer history for composer-focused PageDown near a detached window end", async () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    const input = document.createElement("textarea");
    node.append(input);
    Object.defineProperties(node, {
      clientHeight: { configurable: true, value: 500 },
      scrollHeight: { configurable: true, value: 1_000 },
    });
    node.scrollTop = 500;
    const demands: SessionHistoryPageDemand[] = [];
    const removeListener = addSessionHistoryPageDemandListener((demand) => {
      demands.push(demand);
      completeSessionHistoryPageDemand(demand.requestId, true);
    });
    renderTranscriptDemandHook({
      hasNewerHistory: true,
      hasOlderHistory: false,
      scrollContainerRef: ref,
    });

    await act(async () => {
      input.dispatchEvent(
        new KeyboardEvent("keydown", { bubbles: true, key: "PageDown" }),
      );
      await Promise.resolve();
    });

    expect(demands).toHaveLength(1);
    expect(demands[0]).toMatchObject({
      sessionId: "session-a",
      direction: "newer",
    });
    removeListener();
    cleanup();
  });
});
