// Owns visible-tab layout, observer lifecycle and anchor restoration.
// Does not own keyboard/wheel policy or App integration.
// Split from SessionPaneView.scroll.test.ts.
import { act, renderHook } from "@testing-library/react";
import { useLayoutEffect } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  claimMessageStackBottomRepinAuthority,
  useSessionPaneScrollState,
} from "./SessionPaneView.scroll";
import {
  MESSAGE_STACK_SCROLL_WRITE_EVENT,
  requestMessageStackBottomRepin,
} from "./message-stack-scroll-sync";
import type { Message, Session } from "./types";

import {
  session,
  params,
  installAnimationFrameHarness,
} from "./SessionPaneView.scroll.fixtures";

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("session pane scroll observers", () => {
  function renderVisiblePane(shouldStick = true, isActive = true) {
    const frames = installAnimationFrameHarness(1_000 / 60);
    let height = 1_000;
    const node = document.createElement("section");
    Object.defineProperties(node, {
      clientHeight: { value: 200 },
      scrollHeight: { get: () => height },
      scrollTop: { writable: true, value: shouldStick ? 800 : 400 },
      scrollTo: {
        value: (options: ScrollToOptions) => {
          if (typeof options.top === "number") {
            node.scrollTop = options.top;
          }
        },
      },
    });
    const key = "pane-1:session-history";
    const shared = {
      ...params(session(false)),
      paneScrollPositions: { [key]: { top: node.scrollTop, shouldStick } },
      paneShouldStickToBottomRef: { current: { [key]: shouldStick } },
    };
    const initialProps = {
      isActive,
      isSessionTabActive: false,
      isSending: false,
      showWaitingIndicator: false,
    };
    const beforePaint: number[] = [];
    const hook = renderHook(
      (props: typeof initialProps) => {
        const result = useSessionPaneScrollState({ ...shared, ...props });
        useLayoutEffect(() => {
          beforePaint.push(node.scrollTop);
        });
        return result;
      },
      { initialProps },
    );
    hook.result.current.messageStackRef.current = node;
    const visibleProps = { ...initialProps, isSessionTabActive: true };
    hook.rerender(visibleProps);
    frames.drainAnimationFrames();
    return {
      ...frames,
      hook,
      node,
      visibleProps,
      beforePaint,
      grow: (nextHeight: number) => { height = nextHeight; },
    };
  }

  it("keeps bounded Send follow alive across focus loss but cancels it when hidden", () => {
    const {
      hook, node, visibleProps, animationFrames, drainAnimationFrames, grow,
    } = renderVisiblePane();
    hook.rerender({ ...visibleProps, isSending: true });
    expect(animationFrames.size).toBeGreaterThan(0);
    const pendingFrames = [...animationFrames.keys()];

    hook.rerender({ ...visibleProps, isSending: true, isActive: false });
    expect([...animationFrames.keys()]).toEqual(pendingFrames);
    grow(1_300);
    drainAnimationFrames();
    expect(node.scrollTop).toBe(1_100);
    expect(hook.result.current.liveTailPinned).toBe(true);

    hook.rerender({ ...visibleProps, isActive: false, isSending: false });
    hook.rerender({ ...visibleProps, isActive: false, isSending: true });
    expect(animationFrames.size).toBeGreaterThan(0);
    hook.rerender({
      ...visibleProps,
      isActive: false,
      isSending: true,
      isSessionTabActive: false,
    });
    expect(animationFrames.size).toBe(0);
    grow(1_600);
    drainAnimationFrames();
    expect(node.scrollTop).toBe(1_100);
  });

  it.each([true, false])("repins waiting-indicator growth before paint with focus=%s", (isActive) => {
    const { hook, node, visibleProps, beforePaint, grow } = renderVisiblePane(true, isActive);
    grow(1_120);
    hook.rerender({ ...visibleProps, showWaitingIndicator: true });
    expect(beforePaint[beforePaint.length - 1]).toBe(920);
    expect(node.scrollTop).toBe(920);
    expect(hook.result.current.liveTailPinned).toBe(true);
  });

  it.each([true, false])("does not treat an external sending-status edge as this pane's Send (focus=%s)", (isActive) => {
    const { hook, node, visibleProps, animationFrames, grow } = renderVisiblePane(false, isActive);
    grow(1_120);
    hook.rerender({ ...visibleProps, isSending: true });
    expect(node.scrollTop).toBe(400);
    expect(hook.result.current.liveTailPinned).toBe(false);
    expect(hook.result.current.showNewResponseIndicator).toBe(true);
    expect(hook.result.current.newResponseIndicatorLabel).toBe("New activity");
    expect(animationFrames.size).toBe(0);
  });

  it("does not let a stale scroll-state listener claim bottom-repin authority", () => {
    const detail = { authorityPresent: false };

    expect(
      claimMessageStackBottomRepinAuthority(
        detail,
        "pane-1:session-new",
        "pane-1:session-old",
      ),
    ).toBe(false);
    expect(detail.authorityPresent).toBe(false);

    expect(
      claimMessageStackBottomRepinAuthority(
        detail,
        "pane-1:session-new",
        "pane-1:session-new",
      ),
    ).toBe(true);
    expect(detail.authorityPresent).toBe(true);
  });

  it("lets a visible session pane repin its bottom while another pane has focus", () => {
    const liveSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    let scrollHeight = 1_000;
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, get: () => scrollHeight },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    Object.defineProperty(scrollNode, "scrollTo", {
      configurable: true,
      value: vi.fn((options: ScrollToOptions) => {
        if (typeof options.top === "number") {
          scrollNode.scrollTop = options.top;
        }
      }),
    });
    const hook = renderHook(
      ({ isSessionTabActive }) =>
        useSessionPaneScrollState({
          ...params(liveSession),
          isActive: false,
          isSessionTabActive,
          paneScrollPositions: {
            [scrollStateKey]: { top: 800, shouldStick: true },
          },
          paneShouldStickToBottomRef: {
            current: { [scrollStateKey]: true },
          },
          scrollStateKey,
        }),
      { initialProps: { isSessionTabActive: false } },
    );
    hook.result.current.messageStackRef.current = scrollNode;
    hook.rerender({ isSessionTabActive: true });

    scrollHeight = 1_040;
    let authorityPresent = false;
    act(() => {
      authorityPresent = requestMessageStackBottomRepin(scrollNode, {
        beforePaint: true,
      });
    });

    expect(authorityPresent).toBe(true);
    expect(scrollNode.scrollTop).toBe(840);

    hook.rerender({ isSessionTabActive: false });
    expect(
      requestMessageStackBottomRepin(scrollNode, { beforePaint: true }),
    ).toBe(false);
  });

  it.each([
    { isActive: true, shouldStick: true },
    { isActive: false, shouldStick: true },
    { isActive: true, shouldStick: false },
    { isActive: false, shouldStick: false },
  ])("preserves follow intent through late growth ($isActive focus, $shouldStick follow)", ({ isActive, shouldStick }) => {
    const callbacks = new Set<ResizeObserverCallback>();
    class ResizeObserverHarness {
      constructor(private readonly callback: ResizeObserverCallback) {
        callbacks.add(callback);
      }
      observe() {}
      unobserve() {}
      disconnect() {
        callbacks.delete(this.callback);
      }
    }
    vi.stubGlobal("ResizeObserver", ResizeObserverHarness);
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    let height = 1_000;
    const node = document.createElement("section");
    const page = document.createElement("div");
    page.className = "session-conversation-page";
    const anchor = document.createElement("div");
    anchor.className = "message-slot";
    anchor.dataset.messageId = "message-anchor";
    page.append(anchor);
    let anchorDocumentTop = 350;
    node.append(page);
    Object.defineProperties(node, {
      clientHeight: { value: 200 },
      scrollHeight: { get: () => height },
      scrollTop: { writable: true, value: shouldStick ? 800 : 400 },
      scrollTo: {
        value: (options: ScrollToOptions) => {
          if (typeof options.top === "number") {
            node.scrollTop = options.top;
          }
        },
      },
    });
    page.getBoundingClientRect = () => ({ height } as DOMRect);
    node.getBoundingClientRect = () => ({ top: 0, bottom: 200 } as DOMRect);
    anchor.getBoundingClientRect = () => {
      const top = anchorDocumentTop - node.scrollTop;
      return { top, bottom: top + 300 } as DOMRect;
    };
    const scrollStateKey = "pane-1:session-history";
    const sharedParams = {
      ...params(session(false)),
      isActive,
      paneRootRef: { current: node },
      paneScrollPositions: {
        [scrollStateKey]: {
          top: node.scrollTop,
          shouldStick,
          ...(!shouldStick
            ? { anchor: { messageId: "message-anchor", viewportOffsetPx: -50 } }
            : {}),
        },
      },
      paneShouldStickToBottomRef: { current: { [scrollStateKey]: shouldStick } },
    };
    const hook = renderHook(
      ({ visible }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          isSessionTabActive: visible,
        }),
      { initialProps: { visible: false } },
    );
    hook.result.current.messageStackRef.current = node;
    hook.rerender({ visible: true });
    expect(callbacks.size).toBe(1);

    height = 1_300;
    anchorDocumentTop += 300;
    act(() => {
      callbacks.forEach((callback) => callback([], {} as ResizeObserver));
    });
    expect(node.scrollTop).toBe(shouldStick ? 1_100 : 700);
    if (!shouldStick) expect(anchor.getBoundingClientRect().top).toBe(-50);
    expect(hook.result.current.liveTailPinned).toBe(shouldStick);

    // A hidden tab must release layout ownership, even if it retains FOLLOW.
    hook.rerender({ visible: false });
    height = 1_600;
    act(() => {
      window.dispatchEvent(new Event("resize"));
    });
    expect(callbacks.size).toBe(0);
    expect(node.scrollTop).toBe(shouldStick ? 1_100 : 700);
  });

  it.each([true, false])("repins attached live growth before paint and leaves detached readers alone (focus=%s)", (focus) => {
    let nextAnimationFrameId = 1;
    const animationFrames = new Map<number, FrameRequestCallback>();
    const requestAnimationFrame = vi.fn((callback: FrameRequestCallback) => {
      const frameId = nextAnimationFrameId;
      nextAnimationFrameId += 1;
      animationFrames.set(frameId, callback);
      return frameId;
    });
    vi.stubGlobal("requestAnimationFrame", requestAnimationFrame);
    vi.stubGlobal(
      "cancelAnimationFrame",
      vi.fn((frameId: number) => animationFrames.delete(frameId)),
    );
    let scrollHeight = 1_000;
    const scrollNode = document.createElement("section");
    const conversationPage = document.createElement("div");
    conversationPage.className = "session-conversation-page is-active";
    const transcriptCard = document.createElement("article");
    const liveTail = document.createElement("div");
    liveTail.className = "conversation-live-tail";
    liveTail.setAttribute("data-tail-follow", "attached");
    conversationPage.append(transcriptCard, liveTail);
    scrollNode.append(conversationPage);
    let liveTailContentTop = 900;
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, get: () => scrollHeight },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const scrollTo = vi.fn(
      (optionsOrX?: ScrollToOptions | number, y?: number) => {
        const top = typeof optionsOrX === "number" ? y : optionsOrX?.top;
        if (typeof top === "number") {
          scrollNode.scrollTop = top;
        }
      },
    );
    Object.defineProperty(scrollNode, "scrollTo", {
      configurable: true,
      value: scrollTo as HTMLElement["scrollTo"],
    });
    vi.spyOn(liveTail, "getBoundingClientRect").mockImplementation(() => {
      const top = liveTailContentTop - scrollNode.scrollTop;
      return {
        bottom: top + 60,
        height: 60,
        left: 0,
        right: 600,
        top,
        width: 600,
        x: 0,
        y: top,
        toJSON: () => ({}),
      };
    });
    const scrollWrites: CustomEvent[] = [];
    scrollNode.addEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, (event) => {
      scrollWrites.push(event as CustomEvent);
    });
    const prompt: Message = {
      id: "prompt-current",
      type: "text",
      timestamp: "12:01",
      author: "you",
      text: "Current prompt",
    };
    const activeSession: Session = {
      ...session(false),
      messages: [prompt],
      messageCount: 1,
    };
    const firstReply: Message = {
      id: "reply-current",
      type: "text",
      timestamp: "12:02",
      author: "assistant",
      text: "First reply",
    };
    const paneShouldStickToBottomRef = {
      current: { "pane-1:session-history": true },
    };
    const paneScrollPositions: Record<
      string,
      { top: number; shouldStick: boolean }
    > = {};
    const sharedParams = {
      ...params(activeSession),
      defaultScrollToBottom: true,
      isActive: focus,
      isSessionTabActive: true,
      paneScrollPositions,
      paneShouldStickToBottomRef,
      showWaitingIndicator: true,
    };
    const hook = renderHook(
      ({ currentSession, contentSignature, paneActive, waiting, visible }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession: currentSession,
          isActive: paneActive,
          isSessionTabActive: visible,
          showWaitingIndicator: waiting,
          visibleContentSignature: contentSignature,
          visibleMessageContentSignature: contentSignature,
          visibleLastMessageAuthor:
            currentSession.messages[currentSession.messages.length - 1]?.author,
        }),
      {
        initialProps: {
          visible: false,
          currentSession: activeSession,
          contentSignature: "prompt-current",
          paneActive: false,
          waiting: true,
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;
    hook.rerender({
      visible: true,
      currentSession: activeSession,
      contentSignature: "prompt-current",
      paneActive: focus,
      waiting: true,
    });
    animationFrames.clear();
    requestAnimationFrame.mockClear();
    scrollTo.mockClear();

    const firstTailTop = liveTail.getBoundingClientRect().top;
    scrollHeight = 1_120;
    liveTailContentTop += 120;
    hook.rerender({
      visible: true,
      currentSession: {
        ...activeSession,
        messages: [prompt, firstReply],
        messageCount: 2,
      },
      contentSignature: "reply-current",
      paneActive: focus,
      waiting: true,
    });

    expect(scrollNode.scrollTop).toBe(920);
    expect(liveTail.getBoundingClientRect().top).toBe(firstTailTop);
    expect(scrollTo).toHaveBeenCalledTimes(1);
    expect(requestAnimationFrame).not.toHaveBeenCalled();
    expect(animationFrames.size).toBe(0);

    // A bulk append is one React commit and therefore one pre-paint write,
    // regardless of how many messages arrived in that commit.
    const bulkTailTop = liveTail.getBoundingClientRect().top;
    scrollHeight = 1_360;
    liveTailContentTop += 240;
    scrollTo.mockClear();
    const streamingSession = {
      ...activeSession,
      messages: [
        prompt,
        {
          ...firstReply,
          text: "First reply with another streamed line",
        },
        {
          id: "command-current",
          type: "text" as const,
          timestamp: "12:02",
          author: "assistant" as const,
          text: "First bulk result",
        },
        {
          id: "command-current-2",
          type: "text" as const,
          timestamp: "12:02",
          author: "assistant" as const,
          text: "Second bulk result",
        },
      ],
      messageCount: 4,
    };
    hook.rerender({
      visible: true,
      currentSession: streamingSession,
      contentSignature: "reply-current:stream-2",
      paneActive: focus,
      waiting: true,
    });

    expect(scrollNode.scrollTop).toBe(1_160);
    expect(liveTail.getBoundingClientRect().top).toBe(bulkTailTop);
    expect(scrollTo).toHaveBeenCalledTimes(1);
    expect(scrollTo).toHaveBeenCalledWith({
      behavior: "auto",
      top: 1_160,
    });
    expect(scrollWrites[scrollWrites.length - 1]?.detail.scrollKind).toBe(
      "bottom_follow",
    );
    expect(requestAnimationFrame).not.toHaveBeenCalled();

    // A measured card can grow after the commit's layout effects but before
    // paint. A same-render repin request with new geometry must not be mistaken
    // for the duplicate layout-effect call that was already coalesced.
    const measuredTailTop = liveTail.getBoundingClientRect().top;
    scrollHeight = 1_440;
    liveTailContentTop += 80;
    scrollTo.mockClear();
    act(() => {
      requestMessageStackBottomRepin(scrollNode, { beforePaint: true });
    });
    expect(scrollNode.scrollTop).toBe(1_240);
    expect(liveTail.getBoundingClientRect().top).toBe(measuredTailTop);
    expect(scrollTo).toHaveBeenCalledTimes(1);
    expect(requestAnimationFrame).not.toHaveBeenCalled();

    // A status-only commit has no geometry to correct and schedules no work.
    scrollTo.mockClear();
    hook.rerender({
      visible: true,
      currentSession: streamingSession,
      contentSignature: "reply-current:stream-2",
      paneActive: focus,
      waiting: false,
    });
    expect(scrollTo).not.toHaveBeenCalled();
    expect(requestAnimationFrame).not.toHaveBeenCalled();

    // A shrink may already be browser-clamped. The controller still notifies
    // the virtualizer synchronously but never issues an upward correction.
    scrollHeight = 980;
    scrollNode.scrollTop = 780;
    liveTailContentTop = 880;
    scrollTo.mockClear();
    const scrollWriteCountBeforeShrink = scrollWrites.length;
    hook.rerender({
      visible: true,
      currentSession: {
        ...streamingSession,
        messages: [
          prompt,
          {
            ...firstReply,
            text: "Settled reply",
          },
        ],
      },
      contentSignature: "reply-current:settled",
      paneActive: focus,
      waiting: false,
    });
    expect(scrollNode.scrollTop).toBe(780);
    expect(scrollTo).not.toHaveBeenCalled();
    expect(scrollWrites).toHaveLength(scrollWriteCountBeforeShrink + 1);
    expect(scrollWrites[scrollWrites.length - 1]?.detail.scrollKind).toBe(
      "bottom_follow",
    );

    const nextPrompt: Message = {
      id: "prompt-next",
      type: "text",
      timestamp: "12:03",
      author: "you",
      text: "Next prompt",
    };
    hook.rerender({
      visible: true,
      currentSession: {
        ...activeSession,
        messages: [prompt, firstReply, nextPrompt],
        messageCount: 3,
      },
      contentSignature: "prompt-next",
      paneActive: focus,
      waiting: false,
    });

    paneShouldStickToBottomRef.current["pane-1:session-history"] = false;
    scrollNode.scrollTop = 700;
    scrollHeight = 1_240;
    liveTailContentTop = 1_140;
    scrollTo.mockClear();
    hook.rerender({
      visible: true,
      currentSession: {
        ...activeSession,
        messages: [
          prompt,
          firstReply,
          nextPrompt,
          {
            id: "reply-next",
            type: "text",
            timestamp: "12:04",
            author: "assistant",
            text: "Next reply",
          },
        ],
        messageCount: 4,
      },
      contentSignature: "reply-next",
      paneActive: focus,
      waiting: true,
    });

    expect(scrollNode.scrollTop).toBe(700);
    expect(scrollTo).not.toHaveBeenCalled();
    expect(requestAnimationFrame).not.toHaveBeenCalled();
  });

  it("restores a long non-virtualized message anchor and keeps it fixed through late layout", () => {
    const resizeCallbacks = new Set<ResizeObserverCallback>();
    class ResizeObserverHarness {
      constructor(callback: ResizeObserverCallback) {
        resizeCallbacks.add(callback);
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    vi.stubGlobal(
      "ResizeObserver",
      ResizeObserverHarness as unknown as typeof ResizeObserver,
    );
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const liveSession = session(false);
    const scrollStateKey = "pane-1:session-detached";
    const paneScrollPositions = {
      [scrollStateKey]: {
        anchor: {
          messageId: "message-long",
          viewportOffsetPx: -1_200,
        },
        top: 2_000,
        shouldStick: false,
      },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: false },
    };
    const scrollNode = document.createElement("section");
    const page = document.createElement("div");
    page.className = "session-conversation-page";
    const longMessage = document.createElement("div");
    longMessage.className = "message-slot";
    longMessage.dataset.messageId = "message-long";
    page.append(longMessage);
    scrollNode.append(page);
    let pageHeight = 4_000;
    let messageDocumentTop = 1_000;
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, get: () => pageHeight },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    scrollNode.getBoundingClientRect = () =>
      ({ top: 100, bottom: 300 } as DOMRect);
    page.getBoundingClientRect = () =>
      ({ height: pageHeight } as DOMRect);
    longMessage.getBoundingClientRect = () => {
      const top = 100 + messageDocumentTop - scrollNode.scrollTop;
      return { top, bottom: top + 3_000 } as DOMRect;
    };
    const hook = renderHook(
      ({ isSessionTabActive, activeScrollStateKey }) =>
        useSessionPaneScrollState({
          ...params(liveSession),
          isActive: true,
          isSessionTabActive,
          paneScrollPositions,
          paneShouldStickToBottomRef,
          scrollStateKey: activeScrollStateKey,
        }),
      {
        initialProps: {
          isSessionTabActive: false,
          activeScrollStateKey: "pane-1:control-panel",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;
    hook.rerender({
      isSessionTabActive: true,
      activeScrollStateKey: scrollStateKey,
    });
    expect(scrollNode.scrollTop).toBe(2_200);

    messageDocumentTop += 300;
    pageHeight += 300;
    act(() => {
      resizeCallbacks.forEach((callback) =>
        callback([], {} as ResizeObserver),
      );
    });

    expect(scrollNode.scrollTop).toBe(2_500);
    expect(paneScrollPositions[scrollStateKey]).toEqual({
      anchor: {
        messageId: "message-long",
        viewportOffsetPx: -1_200,
      },
      top: 2_500,
      shouldStick: false,
    });
  });

  it("keeps detached restore convergence alive when another visible pane gains focus", () => {
    let nextAnimationFrameId = 1;
    const animationFrames = new Map<number, FrameRequestCallback>();
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        const frameId = nextAnimationFrameId;
        nextAnimationFrameId += 1;
        animationFrames.set(frameId, callback);
        return frameId;
      }),
    );
    vi.stubGlobal(
      "cancelAnimationFrame",
      vi.fn((frameId: number) => animationFrames.delete(frameId)),
    );
    const liveSession = session(false);
    const detachedKey = "pane-1:session-detached";
    const paneScrollPositions = {
      [detachedKey]: { top: 600, shouldStick: false },
    };
    const paneShouldStickToBottomRef = {
      current: { [detachedKey]: false },
    };
    let scrollHeight = 600;
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, get: () => scrollHeight },
      scrollTop: { configurable: true, writable: true, value: 120 },
    });
    const hook = renderHook(
      ({ isActive, isSessionTabActive, scrollStateKey }) =>
        useSessionPaneScrollState({
          ...params(liveSession),
          isActive,
          isSessionTabActive,
          paneScrollPositions,
          paneShouldStickToBottomRef,
          scrollStateKey,
        }),
      {
        initialProps: {
          isActive: true,
          isSessionTabActive: false,
          scrollStateKey: "pane-1:control-panel",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;
    hook.rerender({
      isActive: true,
      isSessionTabActive: true,
      scrollStateKey: detachedKey,
    });
    hook.rerender({
      isActive: false,
      isSessionTabActive: true,
      scrollStateKey: detachedKey,
    });

    expect(scrollNode.scrollTop).toBe(400);
    expect(animationFrames.size).toBeGreaterThan(0);
    scrollHeight = 900;
    let frameTimestamp = 0;
    let drainedFrames = 0;
    while (animationFrames.size > 0 && drainedFrames < 20) {
      const nextFrame = animationFrames.entries().next().value;
      if (!nextFrame) {
        break;
      }
      animationFrames.delete(nextFrame[0]);
      frameTimestamp += 1000 / 60;
      act(() => nextFrame[1](frameTimestamp));
      drainedFrames += 1;
    }

    expect(scrollNode.scrollTop).toBe(600);
    expect(paneScrollPositions[detachedKey]).toEqual({
      top: 600,
      shouldStick: false,
    });
  });

  it("cancels a detached restore retry when its tab leaves the scroll scope", () => {
    let nextAnimationFrameId = 1;
    const animationFrames = new Map<number, FrameRequestCallback>();
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        const frameId = nextAnimationFrameId;
        nextAnimationFrameId += 1;
        animationFrames.set(frameId, callback);
        return frameId;
      }),
    );
    vi.stubGlobal(
      "cancelAnimationFrame",
      vi.fn((frameId: number) => animationFrames.delete(frameId)),
    );
    const liveSession = session(false);
    const detachedKey = "pane-1:session-detached";
    const paneScrollPositions = {
      [detachedKey]: { top: 600, shouldStick: false },
    };
    let scrollHeight = 600;
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, get: () => scrollHeight },
      scrollTop: { configurable: true, writable: true, value: 120 },
    });
    const hook = renderHook(
      ({ isSessionTabActive, activeScrollStateKey }) =>
        useSessionPaneScrollState({
          ...params(liveSession),
          isActive: true,
          isSessionTabActive,
          paneScrollPositions,
          paneShouldStickToBottomRef: {
            current: { [detachedKey]: false },
          },
          scrollStateKey: activeScrollStateKey,
        }),
      {
        initialProps: {
          isSessionTabActive: false,
          activeScrollStateKey: "pane-1:control-panel",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;

    hook.rerender({
      isSessionTabActive: true,
      activeScrollStateKey: detachedKey,
    });
    expect(scrollNode.scrollTop).toBe(400);
    expect(animationFrames.size).toBeGreaterThan(0);

    hook.rerender({
      isSessionTabActive: false,
      activeScrollStateKey: detachedKey,
    });
    expect(animationFrames.size).toBe(0);
    scrollHeight = 900;

    expect(scrollNode.scrollTop).toBe(400);
    expect(paneScrollPositions[detachedKey]).toEqual({
      top: 600,
      shouldStick: false,
    });
  });

  it("repins the first live-turn frame before later layout observers run", () => {
    let scrollHeight = 1_000;
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, get: () => scrollHeight },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const scrollTo = vi.fn((options: ScrollToOptions) => {
      scrollNode.scrollTop = options.top ?? scrollNode.scrollTop;
    });
    Object.defineProperty(scrollNode, "scrollTo", {
      configurable: true,
      value: scrollTo as HTMLElement["scrollTo"],
    });
    const activeSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const observedScrollTops: number[] = [];
    const sharedParams = {
      ...params(activeSession),
      defaultScrollToBottom: true,
      isActive: true,
      isSessionTabActive: true,
      paneShouldStickToBottomRef: {
        current: { [scrollStateKey]: true },
      },
      scrollStateKey,
    };
    const hook = renderHook(
      ({ isSending }) => {
        const state = useSessionPaneScrollState({
          ...sharedParams,
          isSending,
        });
        useLayoutEffect(() => {
          state.messageStackRef.current = scrollNode;
          if (isSending) {
            observedScrollTops.push(scrollNode.scrollTop);
          }
        }, [isSending, state.messageStackRef]);
        return state;
      },
      { initialProps: { isSending: false } },
    );
    scrollTo.mockClear();

    // LIVE TURN adds height in this commit. The hook's layout effect must move
    // the real viewport before a later layout observer (and therefore paint)
    // can see the old bottom.
    scrollHeight = 1_120;
    hook.rerender({ isSending: true });

    expect(observedScrollTops).toEqual([920]);
    expect(scrollTo).toHaveBeenCalledTimes(1);
    expect(scrollTo).toHaveBeenCalledWith({ behavior: "auto", top: 920 });
  });

  it("keeps a detached viewport stable through approval and waiting activity", () => {
    let nextAnimationFrameId = 1;
    const animationFrames = new Map<number, FrameRequestCallback>();
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn((callback: FrameRequestCallback) => {
        const frameId = nextAnimationFrameId;
        nextAnimationFrameId += 1;
        animationFrames.set(frameId, callback);
        return frameId;
      }),
    );
    vi.stubGlobal(
      "cancelAnimationFrame",
      vi.fn((frameId: number) => animationFrames.delete(frameId)),
    );
    let scrollHeight = 2_000;
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, get: () => scrollHeight },
      scrollTop: { configurable: true, writable: true, value: 520 },
    });
    Object.defineProperty(scrollNode, "scrollTo", {
      configurable: true,
      value: vi.fn((options: ScrollToOptions) => {
        if (typeof options.top === "number") {
          scrollNode.scrollTop = options.top;
        }
      }),
    });
    const activeSession = session(false);
    const paneShouldStickToBottomRef = {
      current: { "pane-1:session-history": false },
    };
    const onScrollToBottomRequestHandled = vi.fn();
    const sharedParams = {
      ...params(activeSession),
      isSessionTabActive: true,
      onScrollToBottomRequestHandled,
      paneShouldStickToBottomRef,
    };
    const hook = renderHook(
      ({ paneActive, request, waiting }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          isActive: paneActive,
          pendingScrollToBottomRequest: request,
          showWaitingIndicator: waiting,
        }),
      {
        initialProps: {
          paneActive: false,
          request: null as {
            reattach?: boolean;
            sessionId: string;
            token: number;
          } | null,
          waiting: false,
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;

    hook.rerender({
      paneActive: true,
      request: { reattach: true, sessionId: activeSession.id, token: 7 },
      waiting: false,
    });

    expect(hook.result.current.liveTailPinned).toBe(false);
    expect(
      paneShouldStickToBottomRef.current["pane-1:session-history"],
    ).toBe(false);
    expect(hook.result.current.showNewResponseIndicator).toBe(true);
    expect(hook.result.current.newResponseIndicatorLabel).toBe("New activity");
    expect(animationFrames.size).toBe(0);
    expect(scrollNode.scrollTop).toBe(520);
    expect(onScrollToBottomRequestHandled).toHaveBeenCalledWith(7);

    scrollHeight = 2_120;
    hook.rerender({
      paneActive: true,
      request: null,
      waiting: true,
    });
    expect(animationFrames.size).toBe(0);
    expect(hook.result.current.liveTailPinned).toBe(false);
    expect(hook.result.current.showNewResponseIndicator).toBe(true);
    expect(scrollNode.scrollTop).toBe(520);
  });
});
