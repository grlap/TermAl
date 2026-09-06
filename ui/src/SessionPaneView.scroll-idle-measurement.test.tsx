// Owns integration coverage from virtualized page measurements through the
// pane's bottom-repin authority. DOM geometry is controlled, not scroll policy.
import { act, cleanup, render, renderHook } from "@testing-library/react";
import { useLayoutEffect } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useSessionPaneScrollState } from "./SessionPaneView.scroll";
import { installAnimationFrameHarness, params, session } from "./SessionPaneView.scroll.fixtures";
import {
  MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT,
  requestMessageStackBottomRepin,
} from "./message-stack-scroll-sync";
import { VirtualizedConversationMessageList } from "./panels/VirtualizedConversationMessageList";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function renderMeasuredConversation() {
  const frames = installAnimationFrameHarness(1_000 / 60);
  const observers = new Map<ResizeObserverCallback, Set<Element>>();
  class ResizeObserverHarness {
    constructor(private readonly callback: ResizeObserverCallback) {
      observers.set(callback, new Set());
    }
    observe(target: Element) { observers.get(this.callback)?.add(target); }
    unobserve(target: Element) { observers.get(this.callback)?.delete(target); }
    disconnect() { observers.delete(this.callback); }
  }
  vi.stubGlobal("ResizeObserver", ResizeObserverHarness);
  let height = 1_024;
  let viewportHeight = 200;
  let top = 824;
  const writes: number[] = [];
  const node = document.createElement("section");
  node.className = "message-stack";
  const page = document.createElement("div");
  page.className = "session-conversation-page";
  node.append(page);
  document.body.append(node);
  Object.defineProperties(node, {
    clientHeight: { get: () => viewportHeight },
    clientWidth: { get: () => 1_000 },
    scrollHeight: { get: () => height },
    scrollTop: {
      get: () => top,
      set: (value: number) => { top = value; writes.push(value); },
    },
    scrollTo: { value: ({ top: value }: ScrollToOptions) => {
      if (typeof value === "number") node.scrollTop = value;
    } },
  });
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
    const measuredHeight = this === node ? viewportHeight : height;
    const measuredTop = this === node ? 0 : -top;
    return {
      height: measuredHeight, width: 1_000, top: measuredTop,
      bottom: measuredTop + measuredHeight, left: 0, right: 1_000,
      x: 0, y: measuredTop, toJSON: () => ({}),
    };
  });
  const activeSession = {
    ...session(false), hasOlderHistory: false, messagesLoaded: true, messageCount: 1,
  };
  const key = "pane-1:session-history";
  const shared = {
    ...params(activeSession),
    paneScrollPositions: { [key]: { top, shouldStick: true } },
    paneShouldStickToBottomRef: { current: { [key]: true } },
  };
  let hookProps = { visible: false, isActive: true, isSending: false, showWaitingIndicator: false };
  const hook = renderHook(({ visible, ...props }) => useSessionPaneScrollState({
    ...shared, ...props, isSessionTabActive: visible,
  }), { initialProps: hookProps });
  hook.result.current.messageStackRef.current = node;
  hookProps = { ...hookProps, visible: true };
  hook.rerender(hookProps);
  frames.drainAnimationFrames();
  const repinRequests = vi.fn();
  node.addEventListener(MESSAGE_STACK_BOTTOM_REPIN_REQUEST_EVENT, repinRequests);
  render(
    <VirtualizedConversationMessageList
      isActive
      sessionId={activeSession.id}
      messages={activeSession.messages}
      scrollContainerRef={hook.result.current.messageStackRef}
      virtualizerHandleRef={hook.result.current.virtualizerHandleRef}
      tailFollowIntent
      renderMessageCard={(message) => <article className="message-card">{message.id}</article>}
      onApprovalDecision={() => {}}
      onUserInputSubmit={async () => {}}
      onMcpElicitationSubmit={() => {}}
      onCodexAppRequestSubmit={() => {}}
      conversationSearchQuery=""
      conversationSearchMatchedItemKeys={new Set()}
      conversationSearchActiveItemKey={null}
    />,
    { container: page },
  );
  frames.drainAnimationFrames();
  writes.length = 0;
  repinRequests.mockClear();

  const notifyResize = (target: Element) => {
    act(() => {
      for (const [callback, targets] of observers) {
        if (targets.has(target)) callback([], {} as ResizeObserver);
      }
    });
  };
  return {
    hook, node, writes, repinRequests,
    updateFlow(props: Partial<typeof hookProps>) {
      hookProps = { ...hookProps, ...props };
      hook.rerender(hookProps);
      frames.drainAnimationFrames();
    },
    resizeViewport(nextHeight: number) {
      viewportHeight = nextHeight;
      act(() => { requestMessageStackBottomRepin(node); });
      frames.drainAnimationFrames();
    },
    jumpToBottom() {
      act(() => hook.result.current.scrollMessageStackToBoundary("bottom"));
      frames.drainAnimationFrames();
    },
    detach() {
      act(() => hook.result.current.scrollMessageStackByPage(-1));
      frames.drainAnimationFrames();
    },
    measure(nextHeight: number, paneObserver: "before" | "after" | "none") {
      height = nextHeight;
      // Model native clamping without mistaking it for an application write.
      top = Math.min(top, height - node.clientHeight);
      if (paneObserver === "before") notifyResize(page);
      const slot = page.querySelector(".virtualized-message-slot");
      expect(slot).not.toBeNull();
      notifyResize(slot!);
      frames.drainAnimationFrames();
      if (paneObserver === "after") notifyResize(page);
      frames.drainAnimationFrames();
    },
    dispose() { cleanup(); node.remove(); },
  };
}

describe("idle measured conversation bottom follow", () => {
  it("retains a page-swap edge across a layout write before observer delivery", () => {
    const frames = installAnimationFrameHarness(1_000 / 60);
    const callbacks = new Set<ResizeObserverCallback>();
    class ResizeObserverHarness {
      constructor(private readonly callback: ResizeObserverCallback) {
        callbacks.add(callback);
      }
      observe() {}
      unobserve() {}
      disconnect() { callbacks.delete(this.callback); }
    }
    vi.stubGlobal("ResizeObserver", ResizeObserverHarness);
    // Deliver the structural change through the resize observer explicitly,
    // after the synchronous layout write, without an extra mutation delivery.
    vi.stubGlobal("MutationObserver", class {
      observe() {}
      disconnect() {}
    });
    const node = document.createElement("section");
    const oldPage = document.createElement("div");
    oldPage.className = "session-conversation-page";
    node.append(oldPage);
    let height = 1_024;
    const writes: number[] = [];
    Object.defineProperties(node, {
      clientHeight: { value: 200 },
      scrollHeight: { get: () => height },
      scrollTop: { value: 824, writable: true },
      scrollTo: { value: ({ top }: ScrollToOptions) => {
        if (typeof top === "number") {
          node.scrollTop = top;
          writes.push(top);
        }
      } },
    });
    oldPage.getBoundingClientRect = () => ({ height: 1_024 } as DOMRect);
    const shared = params({ ...session(false), hasOlderHistory: false });
    const hook = renderHook(({ visible, writeToken }) => {
      const state = useSessionPaneScrollState({
        ...shared, isActive: true, isSessionTabActive: visible,
      });
      useLayoutEffect(() => {
        if (writeToken > 0) state.scrollMessageStackToBoundary("bottom");
      }, [writeToken]);
      return state;
    }, { initialProps: { visible: false, writeToken: 0 } });
    hook.result.current.messageStackRef.current = node;
    hook.rerender({ visible: true, writeToken: 0 });
    frames.drainAnimationFrames();
    writes.length = 0;

    const newPage = document.createElement("div");
    newPage.className = "session-conversation-page";
    newPage.getBoundingClientRect = () => ({ height } as DOMRect);
    node.replaceChildren(newPage);
    height = 1_025;
    hook.rerender({ visible: true, writeToken: 1 });
    expect(writes).toEqual([825]);
    writes.length = 0;

    height = 1_026;
    act(() => callbacks.forEach((callback) => callback([], {} as ResizeObserver)));
    expect(writes).toEqual([826]);
    // The replacement is a one-shot edge, not a permanent deadband bypass.
    writes.length = 0;
    height = 1_027;
    act(() => callbacks.forEach((callback) => callback([], {} as ResizeObserver)));
    expect(writes).toEqual([]);
  });

  it("reuses the resolved conversation page during streaming writes", () => {
    const view = renderMeasuredConversation();
    try {
      view.updateFlow({ showWaitingIndicator: true });
      const queryPage = vi.spyOn(view.node, "querySelector");
      for (const height of [1_025, 1_026, 1_027]) view.measure(height, "none");
      expect(view.node.scrollTop).toBe(827);
      expect(queryPage.mock.calls.filter(([selector]) =>
        selector.includes(".session-conversation-page"))).toEqual([]);
    } finally { view.dispose(); }
  });

  it.each(["none", "before", "after"] as const)(
    "does not write scrollTop for alternating 1-2 px page measurements (pane observer: %s)",
    (paneObserver) => {
      const view = renderMeasuredConversation();
      try {
        for (const delta of [1, 0, 2, 0, -1, 0, -2, 0]) {
          view.measure(1_024 + delta, paneObserver);
          // This is the real measured-page cache, proving that each delivery
          // traversed the virtualizer's height/layout path, not a mocked event.
          expect(view.hook.result.current.virtualizerHandleRef.current
            ?.getLayoutSnapshot().messages[0].measuredPageHeightPx).toBe(1_024 + delta);
        }
        expect(view.repinRequests).toHaveBeenCalled();
        expect(view.writes).toEqual([]);
        expect(view.hook.result.current.liveTailPinned).toBe(true);
        expect(view.hook.result.current.showNewResponseIndicator).toBe(false);
      } finally {
        view.dispose();
      }
    },
  );

  it("accumulates real idle growth across both measurement routes and focus changes", () => {
    const view = renderMeasuredConversation();
    try {
      view.measure(1_025, "none");
      view.updateFlow({ isActive: false });
      view.measure(1_026, "before");
      expect(view.writes).toEqual([]);
      view.measure(1_027, "after");
      expect(view.writes).toEqual([827]);
      view.measure(1_028, "none");
      expect(view.writes).toEqual([827]);
      view.measure(1_030, "before");
      expect(view.writes).toEqual([827, 830]);
      expect(view.hook.result.current.liveTailPinned).toBe(true);
    } finally { view.dispose(); }
  });

  it("does not let idle before-paint layout requests bypass the shared deadband", () => {
    const view = renderMeasuredConversation();
    try {
      view.measure(1_025, "none");
      act(() => {
        expect(requestMessageStackBottomRepin(view.node, { beforePaint: true })).toBe(true);
      });
      expect(view.writes).toEqual([]);
    } finally { view.dispose(); }
  });

  it("follows single-pixel streaming growth and suppresses jitter after completion", () => {
    const view = renderMeasuredConversation();
    try {
      view.updateFlow({ showWaitingIndicator: true });
      view.writes.length = 0;
      view.measure(1_025, "none");
      expect(view.writes).toEqual([825]);
      view.updateFlow({ showWaitingIndicator: false });
      view.writes.length = 0;
      view.measure(1_026, "before");
      view.measure(1_025, "after");
      expect(view.writes).toEqual([]);
    } finally { view.dispose(); }
  });

  it("allows explicit bottom navigation through the deadband and refreshes its baseline", () => {
    const view = renderMeasuredConversation();
    try {
      view.measure(1_026, "none");
      expect(view.writes).toEqual([]);
      view.jumpToBottom();
      expect(view.node.scrollTop).toBe(826);
      expect(view.writes.length).toBeGreaterThan(0);
      view.writes.length = 0;
      view.measure(1_027, "none");
      view.measure(1_028, "before");
      expect(view.writes).toEqual([]);
      view.measure(1_029, "after");
      expect(view.writes).toEqual([829]);
    } finally { view.dispose(); }
  });

  it("still follows a single-pixel viewport resize while idle", () => {
    const view = renderMeasuredConversation();
    try {
      view.resizeViewport(199);
      expect(view.writes).toEqual([825]);
    } finally { view.dispose(); }
  });

  it("does not move or reattach a STAY reader on measured growth", () => {
    const view = renderMeasuredConversation();
    try {
      view.detach();
      expect(view.hook.result.current.liveTailPinned).toBe(false);
      const readerTop = view.node.scrollTop;
      view.writes.length = 0;
      view.measure(1_025, "before");
      view.measure(1_040, "after");
      expect(view.writes).toEqual([]);
      expect(view.node.scrollTop).toBe(readerTop);
      expect(view.hook.result.current.liveTailPinned).toBe(false);
    } finally { view.dispose(); }
  });
});
