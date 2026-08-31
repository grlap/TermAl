import { act, fireEvent, renderHook } from "@testing-library/react";
import type {
  FocusEvent as ReactFocusEvent,
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
  TouchEvent as ReactTouchEvent,
  UIEvent as ReactUIEvent,
} from "react";
import { useLayoutEffect } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  canMoveMessageStackByDelta,
  claimMessageStackBottomRepinAuthority,
  isFirstAgentOutputForObservedPrompt,
  isMessageStackAtPhysicalBottom,
  resolveLatestTurnOutputState,
  resolveLatestTurnTailSignature,
  resolveNewResponseIndicatorVisibility,
  resolvePostLiveMessageFollowTransition,
  resolveSessionBottomFollowPersistedScrollTop,
  resolveSessionBottomFollowScrollTop,
  resolveSessionBottomFollowWriteScrollTop,
  resolveSessionPageScrollDistance,
  useSessionPaneScrollState,
} from "./SessionPaneView.scroll";
import {
  MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS,
  MESSAGE_STACK_POINTER_OWNERSHIP_MS,
  MESSAGE_STACK_SCROLL_WRITE_EVENT,
  MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
  claimMessageStackNativeScrollOwnership,
  consumeMessageStackVirtualizerPositionCorrection,
  markMessageStackVirtualizerPositionCorrection,
  requestMessageStackBottomRepin,
  peekMessageStackNativeScrollOwnership,
} from "./message-stack-scroll-sync";
import {
  addSessionHistoryPageDemandListener,
  completeSessionHistoryPageDemand,
  type SessionHistoryPageDemand,
} from "./session-history-demand";
import type { Message, Session } from "./types";

function session(hasNewerHistory: boolean): Session {
  return {
    id: "session-history",
    name: "History",
    emoji: "H",
    agent: "Codex",
    workdir: "/tmp",
    model: "gpt",
    status: "active",
    preview: "",
    messages: [
      {
        id: hasNewerHistory ? "message-64" : "message-1000",
        type: "text",
        timestamp: "12:00",
        author: "assistant",
        text: hasNewerHistory ? "Historical message" : "Live tail message",
      },
    ],
    messagesLoaded: false,
    hasOlderHistory: !hasNewerHistory,
    hasNewerHistory,
    messageCount: 1_000,
  };
}

function params(activeSession: Session) {
  return {
    activeSession,
    activeSessionSearchMatch: null,
    defaultScrollToBottom: false,
    deferContentScrollEffects: false,
    hasSessionFindQuery: false,
    isActive: false,
    isSending: false,
    isSessionTabActive: false,
    onScrollToBottomRequestHandled: vi.fn(),
    paneContentSignatures: {},
    paneMessageContentSignatures: {},
    paneRootRef: { current: null },
    paneScrollPositions: {},
    paneShouldStickToBottomRef: { current: {} },
    paneViewMode: "session" as const,
    pendingScrollToBottomRequest: null,
    scrollStateKey: "pane-1:session-history",
    showWaitingIndicator: false,
    visibleContentSignature: "history",
    visibleLastMessageAuthor: "assistant" as const,
    visibleMessageContentSignature: "history-message",
  };
}

function withInputTimestamp<T extends Event>(event: T, timeStamp: number) {
  Object.defineProperty(event, "timeStamp", {
    configurable: true,
    value: timeStamp,
  });
  return event;
}

function installAnimationFrameHarness() {
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
  const drainAnimationFrames = () => {
    let drainCount = 0;
    while (animationFrames.size > 0) {
      drainCount += 1;
      if (drainCount > 50) {
        throw new Error(
          `animation frame drain exceeded 50 rounds with ${animationFrames.size} callbacks pending`,
        );
      }
      const callbacks = Array.from(animationFrames.values());
      animationFrames.clear();
      act(() => {
        callbacks.forEach((callback) => callback(performance.now()));
      });
    }
  };
  return { animationFrames, drainAnimationFrames, requestAnimationFrame };
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("session pane historical-window tail state", () => {
  it("records transcript ownership before an inactive pane becomes active", async () => {
    const liveSession = session(false);
    const scrollNode = document.createElement("section");
    const messageCard = document.createElement("article");
    scrollNode.append(messageCard);
    document.body.append(scrollNode);
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const intentListener = vi.fn();
    scrollNode.addEventListener(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      intentListener,
    );
    const hook = renderHook(
      ({ isActive, scrollStateKey }) =>
        useSessionPaneScrollState({
          ...params(liveSession),
          isActive,
          isSessionTabActive: true,
          scrollStateKey,
        }),
      {
        initialProps: {
          isActive: false,
          scrollStateKey: "pane-1:session-history",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;

    try {
      act(() => {
        // This pointer event occurs while the pane is still inactive, exactly
        // as it does before the pane root's onMouseDown activates the pane.
        fireEvent.mouseDown(messageCard);
      });
      const clipboardTextarea = document.createElement("textarea");
      document.body.append(clipboardTextarea);
      await act(async () => {
        clipboardTextarea.focus();
        clipboardTextarea.remove();
        await Promise.resolve();
      });
      const transientOutsideTarget = document.createElement("div");
      document.body.append(transientOutsideTarget);
      const activeElementSpy = vi
        .spyOn(document, "activeElement", "get")
        .mockReturnValue(null);
      try {
        await act(async () => {
          fireEvent.focusIn(transientOutsideTarget);
          await Promise.resolve();
        });
      } finally {
        activeElementSpy.mockRestore();
        transientOutsideTarget.remove();
      }
      hook.rerender({
        isActive: true,
        scrollStateKey: "pane-1:session-history",
      });
      act(() => {
        fireEvent.keyDown(document.body, { key: "ArrowUp" });
      });

      expect(intentListener).toHaveBeenCalledTimes(1);
      scrollNode.scrollTop = 0;
      act(() => {
        // Boundary intent still hydrates older history even though this
        // resident window cannot move. Downward body-owned intent uses the
        // same normalized bridge.
        fireEvent.keyDown(document.body, { key: "ArrowUp" });
        fireEvent.keyDown(document.body, { key: "PageDown" });
      });
      expect(intentListener).toHaveBeenCalledTimes(3);
      expect(
        intentListener.mock.calls.map(
          ([event]) =>
            (event as CustomEvent).detail as {
              direction: string;
              viewportCanMove: boolean;
            },
        ),
      ).toMatchObject([
        { direction: "up", viewportCanMove: true },
        { direction: "up", viewportCanMove: false },
        { direction: "down", viewportCanMove: true },
      ]);
      expect(scrollNode.scrollTop).toBe(170);

      hook.rerender({
        isActive: true,
        scrollStateKey: "pane-1:session-other",
      });
      act(() => {
        fireEvent.keyDown(document.body, { key: "ArrowUp" });
      });
      expect(intentListener).toHaveBeenCalledTimes(3);
    } finally {
      hook.unmount();
      scrollNode.remove();
    }
  });

  it("detaches immovable upward keys only when older history can hydrate", () => {
    const exerciseBoundary = (
      canHydrateOlderHistory: boolean,
      key: "ArrowUp" | "PageUp",
    ) => {
      const activeSession = canHydrateOlderHistory
        ? session(false)
        : {
            ...session(false),
            hasOlderHistory: false,
            hasNewerHistory: false,
            messagesLoaded: true,
          };
      const scrollNode = document.createElement("section");
      const messageCard = document.createElement("article");
      scrollNode.append(messageCard);
      document.body.append(scrollNode);
      Object.defineProperties(scrollNode, {
        clientHeight: { configurable: true, value: 200 },
        scrollHeight: { configurable: true, value: 200 },
        scrollTop: { configurable: true, writable: true, value: 0 },
      });
      const intentListener = vi.fn();
      scrollNode.addEventListener(
        MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
        intentListener,
      );
      const hook = renderHook(() =>
        useSessionPaneScrollState({
          ...params(activeSession),
          isActive: true,
          isSessionTabActive: true,
        }),
      );
      hook.result.current.messageStackRef.current = scrollNode;

      act(() => {
        fireEvent.mouseDown(messageCard);
        fireEvent.keyDown(document.body, { key });
      });

      const result = {
        detail: (intentListener.mock.calls[0]?.[0] as CustomEvent | undefined)
          ?.detail,
        liveTailPinned: hook.result.current.liveTailPinned,
      };
      hook.unmount();
      scrollNode.remove();
      return result;
    };

    expect(exerciseBoundary(true, "ArrowUp")).toMatchObject({
      detail: {
        detachFromBottomAtBoundary: true,
        direction: "up",
        viewportCanMove: false,
      },
      liveTailPinned: false,
    });
    expect(exerciseBoundary(false, "ArrowUp")).toMatchObject({
      detail: {
        detachFromBottomAtBoundary: false,
        direction: "up",
        viewportCanMove: false,
      },
      liveTailPinned: true,
    });
    expect(exerciseBoundary(true, "PageUp")).toMatchObject({
      detail: {
        detachFromBottomAtBoundary: true,
        direction: "up",
        scrollKind: "page_jump",
        viewportCanMove: false,
      },
      liveTailPinned: false,
    });
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

  it("smoothly advances live bottom-follow without issuing an upward correction", () => {
    const regularFrame = resolveSessionBottomFollowScrollTop(800, 1_000);
    const delayedFrame = resolveSessionBottomFollowScrollTop(800, 1_000, 100);

    expect(regularFrame).toBeGreaterThan(800);
    expect(regularFrame).toBeLessThan(1_000);
    expect(delayedFrame).toBeGreaterThan(regularFrame);
    expect(delayedFrame).toBeLessThanOrEqual(1_000);
    expect(resolveSessionBottomFollowScrollTop(1_000, 800)).toBe(1_000);
    expect(resolveSessionBottomFollowScrollTop(999.5, 1_000)).toBe(1_000);
    expect(resolveSessionBottomFollowScrollTop(800.5, 800)).toBe(800.5);
  });

  it("never reverses a before-paint bottom snap when shrink geometry is not clamped yet", () => {
    expect(
      resolveSessionBottomFollowWriteScrollTop({
        currentScrollTop: 920,
        snapBeforePaint: true,
        targetScrollTop: 880,
      }),
    ).toBe(920);
    expect(
      resolveSessionBottomFollowWriteScrollTop({
        currentScrollTop: 800,
        snapBeforePaint: true,
        targetScrollTop: 880,
      }),
    ).toBe(880);
  });

  it("persists a smooth bottom-follow destination instead of its pre-animation read", () => {
    expect(
      resolveSessionBottomFollowPersistedScrollTop({
        behavior: "smooth",
        observedScrollTop: 800,
        writeScrollTop: 920,
        wroteScrollTop: true,
      }),
    ).toBe(920);
    expect(
      resolveSessionBottomFollowPersistedScrollTop({
        behavior: "auto",
        observedScrollTop: 910,
        writeScrollTop: 920,
        wroteScrollTop: true,
      }),
    ).toBe(910);
    expect(
      resolveSessionBottomFollowPersistedScrollTop({
        behavior: "smooth",
        observedScrollTop: 800,
        writeScrollTop: 920,
        wroteScrollTop: false,
      }),
    ).toBe(800);
  });

  it("bounds each frame of a large structural addition and still converges", () => {
    const targetScrollTop = 1_800;
    let currentScrollTop = 800;

    for (let frame = 0; frame < 60; frame += 1) {
      const nextScrollTop = resolveSessionBottomFollowScrollTop(
        currentScrollTop,
        targetScrollTop,
      );
      expect(nextScrollTop).toBeGreaterThanOrEqual(currentScrollTop);
      expect(nextScrollTop - currentScrollTop).toBeLessThan(50);
      currentScrollTop = nextScrollTop;
    }

    expect(currentScrollTop).toBe(targetScrollTop);
  });

  it("identifies the first agent output for the latest prompt", () => {
    const prompt: Message = {
      id: "prompt-2",
      type: "text",
      timestamp: "12:01",
      author: "you",
      text: "Current prompt",
    };
    const reply: Message = {
      id: "reply-2",
      type: "text",
      timestamp: "12:02",
      author: "assistant",
      text: "First reply",
    };

    expect(resolveLatestTurnOutputState([prompt])).toEqual({
      hasAgentOutput: false,
      promptMessageId: "prompt-2",
    });
    expect(resolveLatestTurnOutputState([prompt, reply])).toEqual({
      hasAgentOutput: true,
      promptMessageId: "prompt-2",
    });
    expect(
      resolveLatestTurnOutputState([
        {
          ...reply,
          id: "assistant-only-tail",
        },
      ]),
    ).toEqual({
      hasAgentOutput: true,
      promptMessageId: null,
    });
  });

  it("does not treat an older-history prepend that reveals the prompt as new output", () => {
    expect(
      isFirstAgentOutputForObservedPrompt(
        {
          hasAgentOutput: true,
          promptMessageId: null,
        },
        {
          hasAgentOutput: true,
          promptMessageId: "prompt-outside-old-tail",
        },
      ),
    ).toBe(false);
  });

  it("carries final-message follow across a status-only live-to-idle commit", () => {
    const statusOnlyTransition = resolvePostLiveMessageFollowTransition({
      awaitingPromptMessageId: undefined,
      currentLiveFlowActive: false,
      currentPromptMessageId: "prompt-1",
      latestTurnContentChanged: false,
      previousLiveFlowActive: true,
    });
    expect(statusOnlyTransition).toEqual({
      awaitingPostLivePromptMessageId: "prompt-1",
      shouldFollowPostLiveMessage: false,
    });

    expect(
      resolvePostLiveMessageFollowTransition({
        awaitingPromptMessageId:
          statusOnlyTransition.awaitingPostLivePromptMessageId,
        currentLiveFlowActive: false,
        currentPromptMessageId: "prompt-1",
        latestTurnContentChanged: true,
        previousLiveFlowActive: false,
      }),
    ).toEqual({
      awaitingPostLivePromptMessageId: undefined,
      shouldFollowPostLiveMessage: true,
    });
  });

  it("consumes a same-commit final message and discards stale state on a new live turn", () => {
    expect(
      resolvePostLiveMessageFollowTransition({
        awaitingPromptMessageId: undefined,
        currentLiveFlowActive: false,
        currentPromptMessageId: "prompt-1",
        latestTurnContentChanged: true,
        previousLiveFlowActive: true,
      }),
    ).toEqual({
      awaitingPostLivePromptMessageId: undefined,
      shouldFollowPostLiveMessage: true,
    });
    expect(
      resolvePostLiveMessageFollowTransition({
        awaitingPromptMessageId: "prompt-1",
        currentLiveFlowActive: true,
        currentPromptMessageId: "prompt-1",
        latestTurnContentChanged: false,
        previousLiveFlowActive: false,
      }),
    ).toEqual({
      awaitingPostLivePromptMessageId: undefined,
      shouldFollowPostLiveMessage: false,
    });
  });

  it("discards an aborted turn latch when a later prompt changes the turn identity", () => {
    const abortedTurn = resolvePostLiveMessageFollowTransition({
      awaitingPromptMessageId: undefined,
      currentLiveFlowActive: false,
      currentPromptMessageId: "prompt-1",
      latestTurnContentChanged: false,
      previousLiveFlowActive: true,
    });

    expect(
      resolvePostLiveMessageFollowTransition({
        awaitingPromptMessageId: abortedTurn.awaitingPostLivePromptMessageId,
        currentLiveFlowActive: false,
        currentPromptMessageId: "prompt-2",
        latestTurnContentChanged: true,
        previousLiveFlowActive: false,
      }),
    ).toEqual({
      awaitingPostLivePromptMessageId: undefined,
      shouldFollowPostLiveMessage: false,
    });
  });

  it("keeps the post-live latch across a resident-window trim", () => {
    const prompt: Message = {
      id: "prompt-1",
      type: "text",
      timestamp: "12:00",
      author: "you",
      text: "Prompt",
    };
    const progress: Message = {
      id: "progress-1",
      type: "text",
      timestamp: "12:01",
      author: "assistant",
      text: "Working",
    };
    const beforeTrim = [
      {
        ...progress,
        id: "old-prefix",
        text: "Old prefix",
      },
      prompt,
      progress,
    ];
    const afterTrim = [prompt, progress];
    expect(resolveLatestTurnTailSignature(afterTrim)).toBe(
      resolveLatestTurnTailSignature(beforeTrim),
    );

    const ended = resolvePostLiveMessageFollowTransition({
      awaitingPromptMessageId: undefined,
      currentLiveFlowActive: false,
      currentPromptMessageId: "prompt-1",
      latestTurnContentChanged: false,
      previousLiveFlowActive: true,
    });
    const trimmed = resolvePostLiveMessageFollowTransition({
      awaitingPromptMessageId: ended.awaitingPostLivePromptMessageId,
      currentLiveFlowActive: false,
      currentPromptMessageId: "prompt-1",
      latestTurnContentChanged:
        resolveLatestTurnTailSignature(beforeTrim) !==
        resolveLatestTurnTailSignature(afterTrim),
      previousLiveFlowActive: false,
    });
    expect(trimmed).toEqual({
      awaitingPostLivePromptMessageId: "prompt-1",
      shouldFollowPostLiveMessage: false,
    });

    expect(
      resolvePostLiveMessageFollowTransition({
        awaitingPromptMessageId: trimmed.awaitingPostLivePromptMessageId,
        currentLiveFlowActive: false,
        currentPromptMessageId: "prompt-1",
        latestTurnContentChanged: true,
        previousLiveFlowActive: false,
      }),
    ).toEqual({
      awaitingPostLivePromptMessageId: undefined,
      shouldFollowPostLiveMessage: true,
    });
  });

  it("changes the latest-turn signature when earlier output changes before an unchanged tail", () => {
    const prompt: Message = {
      id: "prompt-1",
      type: "text",
      timestamp: "12:00",
      author: "you",
      text: "Prompt",
    };
    const progress: Message = {
      id: "progress-1",
      type: "text",
      timestamp: "12:01",
      author: "assistant",
      text: "Working",
    };
    const finalMessage: Message = {
      id: "final-1",
      type: "text",
      timestamp: "12:02",
      author: "assistant",
      text: "Done",
    };

    expect(
      resolveLatestTurnTailSignature([prompt, progress, finalMessage]),
    ).not.toBe(
      resolveLatestTurnTailSignature([
        prompt,
        { ...progress, text: "Working harder" },
        finalMessage,
      ]),
    );
  });

  it("changes the latest-turn signature when output is inserted before an unchanged tail", () => {
    const prompt: Message = {
      id: "prompt-1",
      type: "text",
      timestamp: "12:00",
      author: "you",
      text: "Prompt",
    };
    const finalMessage: Message = {
      id: "final-1",
      type: "text",
      timestamp: "12:02",
      author: "assistant",
      text: "Done",
    };

    expect(resolveLatestTurnTailSignature([prompt, finalMessage])).not.toBe(
      resolveLatestTurnTailSignature([
        prompt,
        {
          id: "progress-1",
          type: "text",
          timestamp: "12:01",
          author: "assistant",
          text: "Working",
        },
        finalMessage,
      ]),
    );
  });

  it("ignores changes in a turn before the latest prompt", () => {
    const previousPrompt: Message = {
      id: "prompt-previous",
      type: "text",
      timestamp: "11:00",
      author: "you",
      text: "Previous prompt",
    };
    const previousReply: Message = {
      id: "reply-previous",
      type: "text",
      timestamp: "11:01",
      author: "assistant",
      text: "Previous reply",
    };
    const latestPrompt: Message = {
      id: "prompt-latest",
      type: "text",
      timestamp: "12:00",
      author: "you",
      text: "Latest prompt",
    };
    const latestReply: Message = {
      id: "reply-latest",
      type: "text",
      timestamp: "12:01",
      author: "assistant",
      text: "Latest reply",
    };

    expect(
      resolveLatestTurnTailSignature([
        previousPrompt,
        previousReply,
        latestPrompt,
        latestReply,
      ]),
    ).toBe(
      resolveLatestTurnTailSignature([
        previousPrompt,
        { ...previousReply, text: "Changed previous reply" },
        latestPrompt,
        latestReply,
      ]),
    );
  });

  it("still observes a final-message change marker update", () => {
    const prompt: Message = {
      id: "prompt-1",
      type: "text",
      timestamp: "12:00",
      author: "you",
      text: "Prompt",
    };
    const finalMessage: Message = {
      id: "final-1",
      type: "text",
      timestamp: "12:01",
      author: "assistant",
      text: "Done",
    };

    expect(resolveLatestTurnTailSignature([prompt, finalMessage])).not.toBe(
      resolveLatestTurnTailSignature([
        prompt,
        { ...finalMessage, text: "Done with details" },
      ]),
    );
  });

  it("keeps the no-prompt tail fallback stable across older-history reveal and trim", () => {
    const oldHistory: Message = {
      id: "history-1",
      type: "text",
      timestamp: "11:00",
      author: "assistant",
      text: "Older history",
    };
    const residentTail: Message = {
      id: "tail-1",
      type: "text",
      timestamp: "12:00",
      author: "assistant",
      text: "Resident tail",
    };

    expect(resolveLatestTurnTailSignature([residentTail])).toBe(
      resolveLatestTurnTailSignature([oldHistory, residentTail]),
    );
  });

  it("does not re-pin when older history reveals the prompt behind resident output", () => {
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn(() => 1),
    );
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    let scrollHeight = 1_000;
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, get: () => scrollHeight },
      scrollTop: { configurable: true, writable: true, value: 620 },
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
    const reply: Message = {
      id: "reply-resident",
      type: "text",
      timestamp: "12:02",
      author: "assistant",
      text: "Already resident reply",
    };
    const assistantOnlyTail: Session = {
      ...session(false),
      messages: [reply],
      messageCount: 1_000,
      hasOlderHistory: true,
    };
    const paneShouldStickToBottomRef = {
      current: { "pane-1:session-history": false },
    };
    const sharedParams = {
      ...params(assistantOnlyTail),
      isActive: true,
      isSessionTabActive: true,
      paneShouldStickToBottomRef,
    };
    const hook = renderHook(
      ({ currentSession, contentSignature }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession: currentSession,
          visibleContentSignature: contentSignature,
          visibleMessageContentSignature: contentSignature,
          visibleLastMessageAuthor:
            currentSession.messages[currentSession.messages.length - 1]?.author,
        }),
      {
        initialProps: {
          currentSession: assistantOnlyTail,
          contentSignature: "reply-resident",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;
    scrollTo.mockClear();

    scrollHeight = 1_120;
    hook.rerender({
      currentSession: {
        ...assistantOnlyTail,
        messages: [
          {
            id: "prompt-revealed-by-prepend",
            type: "text",
            timestamp: "12:01",
            author: "you",
            text: "Prompt loaded from older history",
          },
          reply,
        ],
      },
      contentSignature: "prompt-revealed-by-prepend:reply-resident",
    });

    expect(scrollNode.scrollTop).toBe(620);
    expect(scrollTo).not.toHaveBeenCalled();
  });

  it("does not advertise older-history reveal after scrolling away from the live bottom", () => {
    const { drainAnimationFrames } = installAnimationFrameHarness();
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
    const residentTail: Message = {
      id: "tail-resident",
      type: "text",
      timestamp: "12:02",
      author: "assistant",
      text: "Already visible at the live bottom",
    };
    const initialSession: Session = {
      ...session(false),
      messages: [residentTail],
      messageCount: 1_000,
      hasOlderHistory: true,
    };
    const paneShouldStickToBottomRef = {
      current: { "pane-1:session-history": true },
    };
    const sharedParams = {
      ...params(initialSession),
      isActive: true,
      isSessionTabActive: true,
      paneShouldStickToBottomRef,
    };
    const hook = renderHook(
      ({ currentSession, contentSignature, messageSignature }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession: currentSession,
          visibleContentSignature: contentSignature,
          visibleMessageContentSignature: messageSignature,
          visibleLastMessageAuthor:
            currentSession.messages[currentSession.messages.length - 1]?.author,
        }),
      {
        initialProps: {
          currentSession: initialSession,
          contentSignature: "1|tail-resident",
          messageSignature: "1|tail-resident",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;

    const preventDefault = vi.fn();
    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "ArrowUp",
        metaKey: false,
        preventDefault,
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    drainAnimationFrames();
    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(scrollNode.scrollTop).toBe(760);
    expect(hook.result.current.liveTailPinned).toBe(false);
    expect(hook.result.current.showNewResponseIndicator).toBe(false);

    scrollHeight = 1_120;
    const olderMessage: Message = {
      id: "history-revealed",
      type: "text",
      timestamp: "12:01",
      author: "you",
      text: "Prompt revealed from older history",
    };
    hook.rerender({
      currentSession: {
        ...initialSession,
        messages: [olderMessage, residentTail],
      },
      contentSignature: "2|tail-resident",
      messageSignature: "2|tail-resident",
    });
    drainAnimationFrames();

    expect(hook.result.current.showNewResponseIndicator).toBe(false);

    hook.rerender({
      currentSession: initialSession,
      contentSignature: "1|tail-resident",
      messageSignature: "1|tail-resident",
    });
    drainAnimationFrames();

    expect(hook.result.current.showNewResponseIndicator).toBe(false);

    const newTail: Message = {
      id: "tail-new",
      type: "text",
      timestamp: "12:03",
      author: "assistant",
      text: "Actually unseen response",
    };
    hook.rerender({
      currentSession: {
        ...initialSession,
        messages: [olderMessage, residentTail, newTail],
      },
      contentSignature: "3|tail-new",
      messageSignature: "3|tail-new",
    });
    drainAnimationFrames();

    expect(hook.result.current.showNewResponseIndicator).toBe(true);
    expect(hook.result.current.newResponseIndicatorLabel).toBe("New response");
  });

  it.each(["reveal", "trim"] as const)(
    "does not advertise an inactive-tab history %s when the tab returns",
    (historyChange) => {
      installAnimationFrameHarness();
      const olderMessage: Message = {
        id: "history-inactive",
        type: "text",
        timestamp: "12:01",
        author: "you",
        text: "Older resident history",
      };
      const residentTail: Message = {
        id: "tail-inactive",
        type: "text",
        timestamp: "12:02",
        author: "assistant",
        text: "Already visible at the tail",
      };
      const initialMessages =
        historyChange === "reveal"
          ? [residentTail]
          : [olderMessage, residentTail];
      const changedMessages =
        historyChange === "reveal"
          ? [olderMessage, residentTail]
          : [residentTail];
      const initialSignature = initialMessages
        .map((message) => message.id)
        .join("|");
      const changedSignature = changedMessages
        .map((message) => message.id)
        .join("|");
      const initialSession: Session = {
        ...session(false),
        messages: initialMessages,
      };
      const sharedParams = {
        ...params(initialSession),
        isActive: true,
        paneShouldStickToBottomRef: {
          current: { "pane-1:session-history": false },
        },
      };
      const hook = renderHook(
        ({ currentSession, messageSignature, tabActive }) =>
          useSessionPaneScrollState({
            ...sharedParams,
            activeSession: currentSession,
            isSessionTabActive: tabActive,
            visibleContentSignature: messageSignature,
            visibleMessageContentSignature: messageSignature,
            visibleLastMessageAuthor:
              currentSession.messages[currentSession.messages.length - 1]
                ?.author,
          }),
        {
          initialProps: {
            currentSession: initialSession,
            messageSignature: initialSignature,
            tabActive: true,
          },
        },
      );
      const historyChangedSession = {
        ...initialSession,
        messages: changedMessages,
      };

      hook.rerender({
        currentSession: historyChangedSession,
        messageSignature: changedSignature,
        tabActive: false,
      });
      hook.rerender({
        currentSession: historyChangedSession,
        messageSignature: changedSignature,
        tabActive: true,
      });

      expect(hook.result.current.showNewResponseIndicator).toBe(false);
      hook.unmount();
    },
  );

  it.each(["reveal", "trim"] as const)(
    "does not advertise a history %s after an A to B to A session switch",
    (historyChange) => {
      installAnimationFrameHarness();
      const olderMessage: Message = {
        id: "history-after-session-switch",
        type: "text",
        timestamp: "12:01",
        author: "you",
        text: "Older resident history",
      };
      const residentTail: Message = {
        id: "tail-after-session-switch",
        type: "text",
        timestamp: "12:02",
        author: "assistant",
        text: "Already visible at the tail",
      };
      const initialMessages =
        historyChange === "reveal"
          ? [residentTail]
          : [olderMessage, residentTail];
      const changedMessages =
        historyChange === "reveal"
          ? [olderMessage, residentTail]
          : [residentTail];
      const initialSession: Session = {
        ...session(false),
        messages: initialMessages,
      };
      const otherSession: Session = {
        ...session(false),
        id: "session-other",
        messages: [
          {
            id: "other-tail",
            type: "text",
            timestamp: "12:03",
            author: "assistant",
            text: "Other session",
          },
        ],
      };
      const paneContentSignatures: Record<string, string> = {};
      const paneMessageContentSignatures: Record<string, string> = {};
      const paneShouldStickToBottomRef = {
        current: {
          "pane-1:session-history": false,
          "pane-1:session-other": false,
        },
      };
      const sharedParams = {
        ...params(initialSession),
        isActive: true,
        isSessionTabActive: true,
        paneContentSignatures,
        paneMessageContentSignatures,
        paneShouldStickToBottomRef,
      };
      const hook = renderHook(
        ({ currentSession, messageSignature, activeScrollStateKey }) =>
          useSessionPaneScrollState({
            ...sharedParams,
            activeSession: currentSession,
            scrollStateKey: activeScrollStateKey,
            visibleContentSignature: messageSignature,
            visibleMessageContentSignature: messageSignature,
            visibleLastMessageAuthor:
              currentSession.messages[currentSession.messages.length - 1]
                ?.author,
          }),
        {
          initialProps: {
            currentSession: initialSession,
            messageSignature: initialMessages
              .map((message) => message.id)
              .join("|"),
            activeScrollStateKey: "pane-1:session-history",
          },
        },
      );

      hook.rerender({
        currentSession: otherSession,
        messageSignature: "other-tail",
        activeScrollStateKey: "pane-1:session-other",
      });
      hook.rerender({
        currentSession: {
          ...initialSession,
          messages: changedMessages,
        },
        messageSignature: changedMessages
          .map((message) => message.id)
          .join("|"),
        activeScrollStateKey: "pane-1:session-history",
      });

      expect(hook.result.current.showNewResponseIndicator).toBe(false);
      hook.unmount();
    },
  );

  it("does not repin queued-prompt activity while transcript search is active", () => {
    installAnimationFrameHarness();
    const residentTail: Message = {
      id: "tail-during-find",
      type: "text",
      timestamp: "12:02",
      author: "assistant",
      text: "Searchable resident tail",
    };
    const initialSession: Session = {
      ...session(false),
      messages: [residentTail],
      pendingPrompts: [],
    };
    const paneShouldStickToBottomRef = {
      current: { "pane-1:session-history": true },
    };
    const sharedParams = {
      ...params(initialSession),
      hasSessionFindQuery: true,
      isActive: true,
      isSessionTabActive: true,
      paneShouldStickToBottomRef,
      showWaitingIndicator: true,
    };
    const hook = renderHook(
      ({ currentSession, contentSignature, messageSignature }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession: currentSession,
          visibleContentSignature: contentSignature,
          visibleMessageContentSignature: messageSignature,
          visibleLastMessageAuthor:
            currentSession.messages[currentSession.messages.length - 1]
              ?.author,
        }),
      {
        initialProps: {
          currentSession: initialSession,
          contentSignature: "tail-during-find",
          messageSignature: "tail-during-find",
        },
      },
    );

    hook.rerender({
      currentSession: {
        ...initialSession,
        messages: [
          {
            id: "history-during-find",
            type: "text",
            timestamp: "12:01",
            author: "you",
            text: "Older searchable history",
          },
          residentTail,
        ],
        pendingPrompts: [
          {
            id: "prompt-during-find",
            timestamp: "12:03",
            text: "Queued during search",
          },
        ],
      },
      contentSignature: "history|tail|prompt",
      messageSignature: "history|tail",
    });

    expect(hook.result.current.liveTailPinned).toBe(false);
    expect(hook.result.current.showNewResponseIndicator).toBe(true);
    expect(hook.result.current.newResponseIndicatorLabel).toBe("New activity");
  });

  it("does not detach or advertise resident-history reveal during transcript search", () => {
    installAnimationFrameHarness();
    const residentTail: Message = {
      id: "tail-during-history-find",
      type: "text",
      timestamp: "12:02",
      author: "assistant",
      text: "Searchable resident tail",
    };
    const initialSession: Session = {
      ...session(false),
      messages: [residentTail],
    };
    const paneShouldStickToBottomRef = {
      current: { "pane-1:session-history": true },
    };
    const sharedParams = {
      ...params(initialSession),
      hasSessionFindQuery: true,
      isActive: true,
      isSessionTabActive: true,
      paneShouldStickToBottomRef,
    };
    const hook = renderHook(
      ({ currentSession, messageSignature }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession: currentSession,
          visibleContentSignature: messageSignature,
          visibleMessageContentSignature: messageSignature,
          visibleLastMessageAuthor:
            currentSession.messages[currentSession.messages.length - 1]
              ?.author,
        }),
      {
        initialProps: {
          currentSession: initialSession,
          messageSignature: "tail-during-history-find",
        },
      },
    );

    hook.rerender({
      currentSession: {
        ...initialSession,
        messages: [
          {
            id: "history-during-history-find",
            type: "text",
            timestamp: "12:01",
            author: "you",
            text: "Older searchable history",
          },
          residentTail,
        ],
      },
      messageSignature: "history-during-history-find|tail-during-history-find",
    });

    expect(hook.result.current.liveTailPinned).toBe(true);
    expect(hook.result.current.showNewResponseIndicator).toBe(false);
    hook.unmount();
  });

  it("advertises a queued prompt that arrives with an older-history reveal", () => {
    const { drainAnimationFrames } = installAnimationFrameHarness();
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
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
    const residentTail: Message = {
      id: "tail-resident",
      type: "text",
      timestamp: "12:02",
      author: "assistant",
      text: "Already visible at the live bottom",
    };
    const initialSession: Session = {
      ...session(false),
      messages: [residentTail],
      pendingPrompts: [
        {
          id: "prompt-already-queued",
          timestamp: "12:03",
          text: "Already queued",
        },
      ],
    };
    const paneShouldStickToBottomRef = {
      current: { "pane-1:session-history": true },
    };
    const sharedParams = {
      ...params(initialSession),
      isActive: true,
      isSessionTabActive: true,
      paneShouldStickToBottomRef,
      showWaitingIndicator: true,
    };
    const hook = renderHook(
      ({ currentSession, contentSignature, messageSignature }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession: currentSession,
          visibleContentSignature: contentSignature,
          visibleMessageContentSignature: messageSignature,
          visibleLastMessageAuthor:
            currentSession.messages[currentSession.messages.length - 1]?.author,
        }),
      {
        initialProps: {
          currentSession: initialSession,
          contentSignature: "1|tail-resident|prompt-already-queued",
          messageSignature: "1|tail-resident",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;

    act(() => {
      hook.result.current.scrollMessageStackByPage(-1);
    });
    drainAnimationFrames();

    const revealedHistory: Message = {
      id: "history-revealed",
      type: "text",
      timestamp: "12:01",
      author: "you",
      text: "Older prompt revealed by history residency",
    };
    hook.rerender({
      currentSession: {
        ...initialSession,
        messages: [revealedHistory, residentTail],
        pendingPrompts: [
          ...initialSession.pendingPrompts!,
          {
            id: "prompt-newly-queued",
            timestamp: "12:04",
            text: "New activity while detached",
          },
        ],
      },
      contentSignature: "2|tail-resident|prompt-newly-queued",
      messageSignature: "2|tail-resident",
    });
    drainAnimationFrames();

    expect(hook.result.current.showNewResponseIndicator).toBe(true);
    expect(hook.result.current.newResponseIndicatorLabel).toBe("New activity");
  });

  it("keeps an unseen assistant tail classified as a response when a prompt queues in the same commit", () => {
    installAnimationFrameHarness();
    const residentTail: Message = {
      id: "tail-resident-before-coalesced-update",
      type: "text",
      timestamp: "12:02",
      author: "assistant",
      text: "Already visible",
    };
    const initialSession: Session = {
      ...session(false),
      messages: [residentTail],
      pendingPrompts: [],
    };
    const paneShouldStickToBottomRef = {
      current: { "pane-1:session-history": false },
    };
    const sharedParams = {
      ...params(initialSession),
      isActive: true,
      isSessionTabActive: true,
      paneShouldStickToBottomRef,
      showWaitingIndicator: true,
    };
    const hook = renderHook(
      ({ currentSession, contentSignature, messageSignature }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession: currentSession,
          visibleContentSignature: contentSignature,
          visibleMessageContentSignature: messageSignature,
          visibleLastMessageAuthor:
            currentSession.messages[currentSession.messages.length - 1]?.author,
        }),
      {
        initialProps: {
          currentSession: initialSession,
          contentSignature: "tail-resident-before-coalesced-update",
          messageSignature: "tail-resident-before-coalesced-update",
        },
      },
    );
    const unseenResponse: Message = {
      id: "tail-response-coalesced-with-prompt",
      type: "text",
      timestamp: "12:03",
      author: "assistant",
      text: "New response",
    };

    hook.rerender({
      currentSession: {
        ...initialSession,
        messages: [residentTail, unseenResponse],
        pendingPrompts: [
          {
            id: "prompt-coalesced-with-response",
            timestamp: "12:03",
            text: "Queued at the same time",
          },
        ],
      },
      contentSignature:
        "tail-response-coalesced-with-prompt|prompt-coalesced-with-response",
      messageSignature: "tail-response-coalesced-with-prompt",
    });

    expect(hook.result.current.showNewResponseIndicator).toBe(true);
    expect(hook.result.current.newResponseIndicatorLabel).toBe("New response");
  });

  it("advertises unseen responses after the pane transition record drifts while inactive", () => {
    installAnimationFrameHarness();
    const prompt: Message = {
      id: "prompt-before-inactive",
      type: "text",
      timestamp: "12:01",
      author: "you",
      text: "Please continue",
    };
    const reply: Message = {
      id: "reply-while-inactive",
      type: "text",
      timestamp: "12:02",
      author: "assistant",
      text: "Finished while the tab was inactive",
    };
    const initialSession: Session = {
      ...session(false),
      messages: [prompt],
      pendingPrompts: [
        {
          id: "queued-before-inactive",
          timestamp: "12:01",
          text: "Queued before switching tabs",
        },
      ],
    };
    const paneShouldStickToBottomRef = {
      current: { "pane-1:session-history": false },
    };
    const sharedParams = {
      ...params(initialSession),
      isActive: true,
      paneShouldStickToBottomRef,
    };
    const hook = renderHook(
      ({ currentSession, contentSignature, messageSignature, tabActive }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession: currentSession,
          isSessionTabActive: tabActive,
          visibleContentSignature: contentSignature,
          visibleMessageContentSignature: messageSignature,
          visibleLastMessageAuthor:
            currentSession.messages[currentSession.messages.length - 1]?.author,
        }),
      {
        initialProps: {
          currentSession: initialSession,
          contentSignature: "prompt-before-inactive|queued",
          messageSignature: "prompt-before-inactive",
          tabActive: true,
        },
      },
    );
    const repliedSession: Session = {
      ...initialSession,
      messages: [prompt, reply],
    };

    hook.rerender({
      currentSession: repliedSession,
      contentSignature: "reply-while-inactive|queued",
      messageSignature: "reply-while-inactive",
      tabActive: false,
    });
    const settledSession: Session = {
      ...repliedSession,
      pendingPrompts: [],
    };
    hook.rerender({
      currentSession: settledSession,
      contentSignature: "reply-while-inactive|settled",
      messageSignature: "reply-while-inactive",
      tabActive: false,
    });
    hook.rerender({
      currentSession: settledSession,
      contentSignature: "reply-while-inactive|settled",
      messageSignature: "reply-while-inactive",
      tabActive: true,
    });

    expect(hook.result.current.showNewResponseIndicator).toBe(true);
    expect(hook.result.current.newResponseIndicatorLabel).toBe("New response");
  });

  it("advertises unseen responses after deferred content effects outlive the response commit", () => {
    installAnimationFrameHarness();
    const prompt: Message = {
      id: "prompt-before-deferred-effects",
      type: "text",
      timestamp: "12:01",
      author: "you",
      text: "Please continue",
    };
    const reply: Message = {
      id: "reply-during-deferred-effects",
      type: "text",
      timestamp: "12:02",
      author: "assistant",
      text: "Finished while content effects were deferred",
    };
    const initialSession: Session = {
      ...session(false),
      messages: [prompt],
      pendingPrompts: [
        {
          id: "queued-before-deferred-effects",
          timestamp: "12:01",
          text: "Queued before deferral",
        },
      ],
    };
    const sharedParams = {
      ...params(initialSession),
      isActive: true,
      isSessionTabActive: true,
      paneShouldStickToBottomRef: {
        current: { "pane-1:session-history": false },
      },
    };
    const hook = renderHook(
      ({ currentSession, contentSignature, messageSignature, deferEffects }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession: currentSession,
          deferContentScrollEffects: deferEffects,
          visibleContentSignature: contentSignature,
          visibleMessageContentSignature: messageSignature,
          visibleLastMessageAuthor:
            currentSession.messages[currentSession.messages.length - 1]?.author,
        }),
      {
        initialProps: {
          currentSession: initialSession,
          contentSignature: "prompt-before-deferred-effects|queued",
          messageSignature: "prompt-before-deferred-effects",
          deferEffects: false,
        },
      },
    );
    const repliedSession: Session = {
      ...initialSession,
      messages: [prompt, reply],
    };

    hook.rerender({
      currentSession: repliedSession,
      contentSignature: "reply-during-deferred-effects|queued",
      messageSignature: "reply-during-deferred-effects",
      deferEffects: true,
    });
    const settledSession: Session = {
      ...repliedSession,
      pendingPrompts: [],
    };
    hook.rerender({
      currentSession: settledSession,
      contentSignature: "reply-during-deferred-effects|settled",
      messageSignature: "reply-during-deferred-effects",
      deferEffects: true,
    });
    hook.rerender({
      currentSession: settledSession,
      contentSignature: "reply-during-deferred-effects|settled",
      messageSignature: "reply-during-deferred-effects",
      deferEffects: false,
    });

    expect(hook.result.current.showNewResponseIndicator).toBe(true);
    expect(hook.result.current.newResponseIndicatorLabel).toBe("New response");
  });

  it("repins attached live growth before paint and leaves detached readers alone", () => {
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
      isActive: true,
      isSessionTabActive: true,
      paneScrollPositions,
      paneShouldStickToBottomRef,
      showWaitingIndicator: true,
    };
    const hook = renderHook(
      ({ currentSession, contentSignature, paneActive, waiting }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession: currentSession,
          isActive: paneActive,
          showWaitingIndicator: waiting,
          visibleContentSignature: contentSignature,
          visibleMessageContentSignature: contentSignature,
          visibleLastMessageAuthor:
            currentSession.messages[currentSession.messages.length - 1]?.author,
        }),
      {
        initialProps: {
          currentSession: activeSession,
          contentSignature: "prompt-current",
          paneActive: false,
          waiting: true,
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;
    hook.rerender({
      currentSession: activeSession,
      contentSignature: "prompt-current",
      paneActive: true,
      waiting: true,
    });
    animationFrames.clear();
    requestAnimationFrame.mockClear();
    scrollTo.mockClear();

    const firstTailTop = liveTail.getBoundingClientRect().top;
    scrollHeight = 1_120;
    liveTailContentTop += 120;
    hook.rerender({
      currentSession: {
        ...activeSession,
        messages: [prompt, firstReply],
        messageCount: 2,
      },
      contentSignature: "reply-current",
      paneActive: true,
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
      currentSession: streamingSession,
      contentSignature: "reply-current:stream-2",
      paneActive: true,
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
      currentSession: streamingSession,
      contentSignature: "reply-current:stream-2",
      paneActive: true,
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
      paneActive: true,
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
      currentSession: {
        ...activeSession,
        messages: [prompt, firstReply, nextPrompt],
        messageCount: 3,
      },
      contentSignature: "prompt-next",
      paneActive: true,
      waiting: false,
    });

    paneShouldStickToBottomRef.current["pane-1:session-history"] = false;
    scrollNode.scrollTop = 700;
    scrollHeight = 1_240;
    liveTailContentTop = 1_140;
    scrollTo.mockClear();
    hook.rerender({
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
      paneActive: true,
      waiting: true,
    });

    expect(scrollNode.scrollTop).toBe(700);
    expect(scrollTo).not.toHaveBeenCalled();
    expect(requestAnimationFrame).not.toHaveBeenCalled();
  });

  it("uses one viewport-relative distance for every session PageUp/PageDown path", () => {
    expect(resolveSessionPageScrollDistance(1_000)).toBe(850);
    expect(resolveSessionPageScrollDistance(100)).toBe(160);
  });

  it("only treats a wheel or touch delta as navigation when scrollTop can move", () => {
    expect(canMoveMessageStackByDelta(0, 100, 200, 20)).toBe(false);
    expect(canMoveMessageStackByDelta(800, 1_000, 200, 20)).toBe(false);
    expect(canMoveMessageStackByDelta(800, 1_000, 200, -20)).toBe(true);
    expect(canMoveMessageStackByDelta(0, 1_000, 200, -20)).toBe(false);
    expect(canMoveMessageStackByDelta(400, 1_000, 200, 20)).toBe(true);
    expect(canMoveMessageStackByDelta(400, 1_000, 200, -20)).toBe(true);
  });

  it("recognizes the reachable physical bottom under fractional zoom geometry", () => {
    expect(isMessageStackAtPhysicalBottom(796.5, 1_000, 200)).toBe(true);
    expect(isMessageStackAtPhysicalBottom(795.5, 1_000, 200)).toBe(false);
  });

  it("moves the transcript and live tail by the same first upward wheel delta", () => {
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

    const activeSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: { top: 800, shouldStick: true },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: true },
    };
    const scrollNode = document.createElement("section");
    const conversationPage = document.createElement("div");
    conversationPage.className = "session-conversation-page is-active";
    const transcriptCard = document.createElement("article");
    transcriptCard.className = "conversation-message-entry-reveal";
    const liveTail = document.createElement("div");
    liveTail.className = "conversation-live-tail";
    liveTail.setAttribute("data-tail-follow", "attached");
    conversationPage.append(transcriptCard, liveTail);
    scrollNode.append(conversationPage);
    let scrollTop = 800;
    const scrollTopWrites: number[] = [];
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: {
        configurable: true,
        get: () => scrollTop,
        set: (nextValue: number) => {
          scrollTop = nextValue;
          scrollTopWrites.push(nextValue);
        },
      },
    });
    const mockFlowRect = (node: HTMLElement, contentTop: number) =>
      vi.spyOn(node, "getBoundingClientRect").mockImplementation(() => {
        const top = contentTop - scrollTop;
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
    mockFlowRect(transcriptCard, 820);
    mockFlowRect(liveTail, 920);

    const hook = renderHook(
      ({ isSessionTabActive }) => {
        const state = useSessionPaneScrollState({
          ...params(activeSession),
          isSessionTabActive,
          paneScrollPositions,
          paneShouldStickToBottomRef,
          scrollStateKey,
        });
        // The component assigns this DOM ref during commit, before the hook's
        // passive native-wheel subscription runs. Mirror that ordering here.
        useLayoutEffect(() => {
          state.messageStackRef.current = scrollNode;
        }, [state.messageStackRef]);
        return state;
      },
      { initialProps: { isSessionTabActive: true } },
    );

    const transcriptTopBefore = transcriptCard.getBoundingClientRect().top;
    const liveTailTopBefore = liveTail.getBoundingClientRect().top;
    const wheelEvent = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      deltaY: -20,
    });
    act(() => {
      scrollNode.dispatchEvent(wheelEvent);
    });
    const transcriptDelta =
      transcriptCard.getBoundingClientRect().top - transcriptTopBefore;
    const liveTailDelta =
      liveTail.getBoundingClientRect().top - liveTailTopBefore;

    expect(wheelEvent.defaultPrevented).toBe(true);
    expect(scrollTopWrites).toEqual([780]);
    expect(scrollNode.scrollTop).toBe(780);
    expect(transcriptDelta).toBe(20);
    expect(liveTailDelta).toBe(transcriptDelta);
    expect(transcriptCard).not.toHaveClass(
      "conversation-message-entry-reveal",
    );
    expect(transcriptCard).toHaveAttribute(
      "data-conversation-message-entry-reveal-cancelled",
    );
    expect(paneScrollPositions[scrollStateKey]).toEqual({
      top: 780,
      shouldStick: false,
    });
    expect(hook.result.current.liveTailPinned).toBe(false);

    hook.rerender({ isSessionTabActive: false });
    hook.rerender({ isSessionTabActive: true });
    expect(scrollNode.scrollTop).toBe(780);
    expect(paneScrollPositions[scrollStateKey]).toEqual({
      top: 780,
      shouldStick: false,
    });
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(false);
    expect(hook.result.current.liveTailPinned).toBe(false);
  });

  it.each([
    ["a real recorded bottom", false],
    ["the pinned-bottom sentinel", true],
  ])(
    "detaches an unannounced upward native scroll from %s before a repin",
    (_recordedPosition, usePinnedBottomSentinel) => {
      vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
      vi.stubGlobal("cancelAnimationFrame", vi.fn());
      const activeSession = session(false);
      const scrollStateKey = "pane-1:session-history";
      const paneScrollPositions = {
        [scrollStateKey]: { top: 800, shouldStick: true },
      };
      const paneShouldStickToBottomRef = {
        current: { [scrollStateKey]: true },
      };
      const scrollNode = document.createElement("section");
      Object.defineProperties(scrollNode, {
        clientHeight: { configurable: true, value: 200 },
        scrollHeight: { configurable: true, value: 1_000 },
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
      const userScrollIntents: Array<Record<string, unknown>> = [];
      scrollNode.addEventListener(
        MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
        (event) => {
          userScrollIntents.push(
            (event as CustomEvent<Record<string, unknown>>).detail,
          );
        },
      );

      const hook = renderHook(() => {
        const state = useSessionPaneScrollState({
          ...params(activeSession),
          isSessionTabActive: true,
          paneScrollPositions,
          paneShouldStickToBottomRef,
          scrollStateKey,
        });
        // Publish the DOM node during render so the hook's layout listeners
        // observe it on their first commit, matching a mounted message stack.
        state.messageStackRef.current = scrollNode;
        return state;
      });

      act(() => {
        // Prime the attached geometry exactly as a wheel-to-bottom scroll
        // frame does in production. The upward frame below deliberately has
        // no keydown, wheel, pointer, or normalized-intent predecessor.
        hook.result.current.handleMessageStackScroll({
          currentTarget: scrollNode,
        } as ReactUIEvent<HTMLElement>);
      });
      if (usePinnedBottomSentinel) {
        paneScrollPositions[scrollStateKey].top = Number.MAX_SAFE_INTEGER;
      }

      act(() => {
        scrollNode.scrollTop = 760;
        hook.result.current.handleMessageStackScroll({
          currentTarget: scrollNode,
        } as ReactUIEvent<HTMLElement>);
      });

      expect(paneScrollPositions[scrollStateKey]).toEqual({
        top: 760,
        shouldStick: false,
      });
      expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(false);
      expect(hook.result.current.liveTailPinned).toBe(false);
      expect(userScrollIntents).toEqual([
        {
          detachFromBottomAtBoundary: false,
          direction: "up",
          scrollKind: "incremental",
          viewportCanMove: true,
        },
      ]);

      act(() => {
        expect(
          requestMessageStackBottomRepin(scrollNode, { beforePaint: true }),
        ).toBe(true);
      });
      expect(scrollNode.scrollTop).toBe(760);
    },
  );

  it.each([
    ["a real recorded bottom", false],
    ["the pinned-bottom sentinel", true],
  ])(
    "keeps tail-follow attached when a taller viewport clamps scrollTop below %s",
    (_recordedPosition, usePinnedBottomSentinel) => {
      vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
      vi.stubGlobal("cancelAnimationFrame", vi.fn());
      const activeSession = session(false);
      const scrollStateKey = "pane-1:session-history";
      const paneScrollPositions = {
        [scrollStateKey]: { top: 800, shouldStick: true },
      };
      const paneShouldStickToBottomRef = {
        current: { [scrollStateKey]: true },
      };
      const scrollNode = document.createElement("section");
      Object.defineProperties(scrollNode, {
        clientHeight: { configurable: true, writable: true, value: 200 },
        scrollHeight: { configurable: true, value: 1_000 },
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
      const userScrollIntents: Array<Record<string, unknown>> = [];
      scrollNode.addEventListener(
        MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
        (event) => {
          userScrollIntents.push(
            (event as CustomEvent<Record<string, unknown>>).detail,
          );
        },
      );

      const hook = renderHook(() => {
        const state = useSessionPaneScrollState({
          ...params(activeSession),
          isSessionTabActive: true,
          paneScrollPositions,
          paneShouldStickToBottomRef,
          scrollStateKey,
        });
        state.messageStackRef.current = scrollNode;
        return state;
      });

      act(() => {
        // Prime an attached bottom frame with the smaller viewport.
        hook.result.current.handleMessageStackScroll({
          currentTarget: scrollNode,
        } as ReactUIEvent<HTMLElement>);
      });
      if (usePinnedBottomSentinel) {
        paneScrollPositions[scrollStateKey].top = Number.MAX_SAFE_INTEGER;
      }

      act(() => {
        // The composer or a pending card shrank: the viewport grew by 60px
        // and the browser clamped scrollTop to the new maximum. No reader
        // input preceded this native frame.
        Object.defineProperty(scrollNode, "clientHeight", {
          configurable: true,
          writable: true,
          value: 260,
        });
        scrollNode.scrollTop = 740;
        hook.result.current.handleMessageStackScroll({
          currentTarget: scrollNode,
        } as ReactUIEvent<HTMLElement>);
      });

      expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(true);
      expect(paneScrollPositions[scrollStateKey].shouldStick).toBe(true);
      expect(hook.result.current.liveTailPinned).toBe(true);
      expect(userScrollIntents).toEqual([]);
    },
  );

  it("keeps tail-follow attached when a one-pixel drop still lands at the physical bottom", () => {
    // Observed in a real browser: the footer height jittered by one pixel
    // between two frames, the browser clamped scrollTop from 2550 to 2549 and
    // reported the same content height and viewport height again. The
    // recorded geometry cannot witness that jitter, so the frame must be
    // classified by where it lands, not by the sub-pixel epsilon.
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const activeSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: { top: 2_550, shouldStick: true },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: true },
    };
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 574 },
      scrollHeight: { configurable: true, value: 3_124 },
      scrollTop: { configurable: true, writable: true, value: 2_550 },
    });
    Object.defineProperty(scrollNode, "scrollTo", {
      configurable: true,
      value: vi.fn((options: ScrollToOptions) => {
        if (typeof options.top === "number") {
          scrollNode.scrollTop = options.top;
        }
      }),
    });
    const userScrollIntents: Array<Record<string, unknown>> = [];
    scrollNode.addEventListener(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      (event) => {
        userScrollIntents.push(
          (event as CustomEvent<Record<string, unknown>>).detail,
        );
      },
    );

    const hook = renderHook(() => {
      const state = useSessionPaneScrollState({
        ...params(activeSession),
        isSessionTabActive: true,
        paneScrollPositions,
        paneShouldStickToBottomRef,
        scrollStateKey,
      });
      state.messageStackRef.current = scrollNode;
      return state;
    });

    act(() => {
      // Prime an attached bottom frame with a real recorded top.
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });
    expect(paneScrollPositions[scrollStateKey]).toEqual({
      top: 2_550,
      shouldStick: true,
    });

    act(() => {
      scrollNode.scrollTop = 2_549;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(true);
    expect(paneScrollPositions[scrollStateKey].shouldStick).toBe(true);
    expect(hook.result.current.liveTailPinned).toBe(true);
    expect(userScrollIntents).toEqual([]);
  });

  it("keeps tail-follow attached when sending a draft grows the viewport past the recorded height", () => {
    // The reader was attached while a multi-line draft kept the composer tall
    // (recorded at a 574px viewport). Sending the draft collapses the composer
    // to one line: the viewport grows to 640px and the browser clamps
    // scrollTop down by that growth without any reader input. The frame lands
    // at the physical bottom and must keep the reader attached.
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const activeSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: { top: 2_550, shouldStick: true },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: true },
    };
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, writable: true, value: 574 },
      scrollHeight: { configurable: true, value: 3_124 },
      scrollTop: { configurable: true, writable: true, value: 2_550 },
    });
    Object.defineProperty(scrollNode, "scrollTo", {
      configurable: true,
      value: vi.fn((options: ScrollToOptions) => {
        if (typeof options.top === "number") {
          scrollNode.scrollTop = options.top;
        }
      }),
    });
    const userScrollIntents: Array<Record<string, unknown>> = [];
    scrollNode.addEventListener(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      (event) => {
        userScrollIntents.push(
          (event as CustomEvent<Record<string, unknown>>).detail,
        );
      },
    );

    const hook = renderHook(() => {
      const state = useSessionPaneScrollState({
        ...params(activeSession),
        isSessionTabActive: true,
        paneScrollPositions,
        paneShouldStickToBottomRef,
        scrollStateKey,
      });
      state.messageStackRef.current = scrollNode;
      return state;
    });

    act(() => {
      // Attached bottom frame recorded while the tall draft kept the viewport
      // at 574px.
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    act(() => {
      // The draft is sent: the composer collapses, the viewport grows to
      // 640px, and the browser clamps scrollTop from 2550 down to the new
      // maximum 2484 in one native frame.
      Object.defineProperty(scrollNode, "clientHeight", {
        configurable: true,
        writable: true,
        value: 640,
      });
      scrollNode.scrollTop = 2_484;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(true);
    expect(paneScrollPositions[scrollStateKey].shouldStick).toBe(true);
    expect(hook.result.current.liveTailPinned).toBe(true);
    expect(userScrollIntents).toEqual([]);
  });

  it.each([
    ["upward", 1_040, 800, 810, false],
    ["downward", 1_080, 850, 880, true],
  ] as const)(
    "%s native motion during a programmatic bottom-follow window yields only to reader movement",
    (_direction, nextScrollHeight, nextScrollTop, expectedTop, expectedPinned) => {
      vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
      vi.stubGlobal("cancelAnimationFrame", vi.fn());
      const activeSession = session(false);
      const scrollStateKey = "pane-1:session-history";
      const paneScrollPositions = {
        [scrollStateKey]: { top: 800, shouldStick: true },
      };
      const paneShouldStickToBottomRef = {
        current: { [scrollStateKey]: true },
      };
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

      const hook = renderHook(() => {
        const state = useSessionPaneScrollState({
          ...params(activeSession),
          isSessionTabActive: true,
          paneScrollPositions,
          paneShouldStickToBottomRef,
          scrollStateKey,
        });
        state.messageStackRef.current = scrollNode;
        return state;
      });

      act(() => {
        hook.result.current.handleMessageStackScroll({
          currentTarget: scrollNode,
        } as ReactUIEvent<HTMLElement>);
      });
      scrollHeight = 1_040;
      act(() => {
        expect(
          requestMessageStackBottomRepin(scrollNode, { beforePaint: true }),
        ).toBe(true);
      });
      expect(scrollNode.scrollTop).toBe(840);

      scrollHeight = nextScrollHeight;
      act(() => {
        scrollNode.scrollTop = nextScrollTop;
        hook.result.current.handleMessageStackScroll({
          currentTarget: scrollNode,
        } as ReactUIEvent<HTMLElement>);
      });

      if (!expectedPinned) {
        // A later downward frame proves the old programmatic window was
        // cancelled rather than merely skipped for one event.
        act(() => {
          scrollNode.scrollTop = expectedTop;
          hook.result.current.handleMessageStackScroll({
            currentTarget: scrollNode,
          } as ReactUIEvent<HTMLElement>);
        });
      }
      expect(paneScrollPositions[scrollStateKey]).toEqual({
        top: expectedTop,
        shouldStick: expectedPinned,
      });
      expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(
        expectedPinned,
      );
      expect(hook.result.current.liveTailPinned).toBe(expectedPinned);
    },
  );

  it.each([
    ["live content growth", 1_040, 800, 840],
    ["a browser clamp after content shrink", 960, 760, 760],
  ])(
    "keeps tail-follow attached through %s",
    (_transition, nextScrollHeight, nextScrollTop, expectedBottom) => {
      vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
      vi.stubGlobal("cancelAnimationFrame", vi.fn());
      const activeSession = session(false);
      const scrollStateKey = "pane-1:session-history";
      const paneScrollPositions = {
        [scrollStateKey]: { top: 800, shouldStick: true },
      };
      const paneShouldStickToBottomRef = {
        current: { [scrollStateKey]: true },
      };
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

      const hook = renderHook(() => {
        const state = useSessionPaneScrollState({
          ...params(activeSession),
          isSessionTabActive: true,
          paneScrollPositions,
          paneShouldStickToBottomRef,
          scrollStateKey,
        });
        state.messageStackRef.current = scrollNode;
        return state;
      });

      act(() => {
        hook.result.current.handleMessageStackScroll({
          currentTarget: scrollNode,
        } as ReactUIEvent<HTMLElement>);
        scrollHeight = nextScrollHeight;
        scrollNode.scrollTop = nextScrollTop;
        hook.result.current.handleMessageStackScroll({
          currentTarget: scrollNode,
        } as ReactUIEvent<HTMLElement>);
      });

      expect(paneScrollPositions[scrollStateKey]).toEqual({
        top: nextScrollTop,
        shouldStick: true,
      });
      expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(true);
      expect(hook.result.current.liveTailPinned).toBe(true);

      act(() => {
        expect(
          requestMessageStackBottomRepin(scrollNode, { beforePaint: true }),
        ).toBe(true);
      });
      expect(scrollNode.scrollTop).toBe(expectedBottom);
    },
  );

  it("persists detached intent after an upward native scrollbar drag inside the near-bottom band", () => {
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
    const activeSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: { top: 790, shouldStick: true },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: true },
    };
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 790 },
    });
    const hook = renderHook(
      ({ isSessionTabActive }) => {
        const state = useSessionPaneScrollState({
          ...params(activeSession),
          isSessionTabActive,
          paneScrollPositions,
          paneShouldStickToBottomRef,
          scrollStateKey,
        });
        useLayoutEffect(() => {
          state.messageStackRef.current = scrollNode;
        }, [state.messageStackRef]);
        return state;
      },
      { initialProps: { isSessionTabActive: true } },
    );

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        currentTarget: scrollNode,
        defaultPrevented: false,
        target: scrollNode,
        type: "mousedown",
      } as unknown as ReactMouseEvent<HTMLElement>);
      // A lower scrollTop than the recorded 790 exercises the
      // movedUpAfterUserEscape persistence branch.
      scrollNode.scrollTop = 780;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    expect(paneScrollPositions[scrollStateKey]).toEqual({
      top: 780,
      shouldStick: false,
    });
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(false);

    hook.rerender({ isSessionTabActive: false });
    hook.rerender({ isSessionTabActive: true });
    expect(scrollNode.scrollTop).toBe(780);
    expect(paneScrollPositions[scrollStateKey]).toEqual({
      top: 780,
      shouldStick: false,
    });
    expect(hook.result.current.liveTailPinned).toBe(false);
  });

  it("owns direct ArrowUp and reattaches only after a native move back to bottom", () => {
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const activeSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: true },
    };
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const virtualizedList = document.createElement("div");
    virtualizedList.className = "virtualized-message-list";
    const visibleSlot = document.createElement("div");
    visibleSlot.className = "virtualized-message-slot";
    visibleSlot.dataset.messageId = "message-visible";
    virtualizedList.append(visibleSlot);
    scrollNode.append(virtualizedList);
    scrollNode.getBoundingClientRect = () =>
      ({ top: 100, bottom: 300 } as DOMRect);
    visibleSlot.getBoundingClientRect = () =>
      ({ top: 124, bottom: 204 } as DOMRect);
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState({
        ...params(activeSession),
        paneScrollPositions,
        paneShouldStickToBottomRef,
        scrollStateKey,
      });
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    const preventDefault = vi.fn();
    const writeListener = vi.fn();
    scrollNode.addEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, writeListener);
    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "ArrowUp",
        metaKey: false,
        preventDefault,
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(scrollNode.scrollTop).toBe(760);
    expect(writeListener).toHaveBeenCalledTimes(1);
    expect((writeListener.mock.calls[0]?.[0] as CustomEvent).detail).toEqual({
      scrollKind: "incremental",
      scrollSource: "user",
    });
    expect(paneScrollPositions[scrollStateKey]).toEqual({
      anchor: {
        messageId: "message-visible",
        viewportOffsetPx: 24,
      },
      top: 760,
      shouldStick: false,
    });
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(false);
    expect(hook.result.current.liveTailPinned).toBe(false);

    act(() => {
      // A scrollbar-thumb or touch move can return without a pane-owned write.
      // Positive movement against the recorded stable geometry is genuine
      // reader intent and may reattach at the reachable physical bottom.
      hook.result.current.handleMessageStackUserScrollIntent({
        currentTarget: scrollNode,
        defaultPrevented: false,
        target: scrollNode,
        type: "mousedown",
      } as unknown as ReactMouseEvent<HTMLElement>);
      scrollNode.scrollTop = 800;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });
    expect(paneScrollPositions[scrollStateKey]?.shouldStick).toBe(true);
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(true);
    expect(hook.result.current.liveTailPinned).toBe(true);
  });

  it("rejects a residual downward wheel tick after ArrowUp escapes the physical bottom", () => {
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    let now = 1_000;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    const activeSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: {
        top: 800,
        shouldStick: true,
      },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: true },
    };
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState({
        ...params(activeSession),
        paneScrollPositions,
        paneShouldStickToBottomRef,
        scrollStateKey,
      });
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    now += 2;
    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "ArrowUp",
        metaKey: false,
        nativeEvent: { timeStamp: now } as KeyboardEvent,
        preventDefault: vi.fn(),
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(scrollNode.scrollTop).toBe(760);
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(false);
    expect(hook.result.current.liveTailPinned).toBe(false);

    now += 3;
    const firstResidualWheel = withInputTimestamp(
      new WheelEvent("wheel", {
        bubbles: true,
        // Chromium may expose later ticks in one wheel sequence as
        // non-cancelable even with a non-passive listener.
        cancelable: false,
        deltaY: 40,
      }),
      now,
    );
    act(() => {
      // A final tick from the superseded downward wheel arrives after ArrowUp.
      // It must not inherit the old direction and reclaim bottom authority.
      scrollNode.dispatchEvent(firstResidualWheel);
      // Non-cancelable Chromium wheel events may still apply their native
      // scroll after dispatch. The explicit superseded-wheel token owns that
      // later native frame and restores the ArrowUp destination.
      scrollNode.scrollTop = 800;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    expect(firstResidualWheel.defaultPrevented).toBe(false);
    expect(scrollNode.scrollTop).toBe(760);
    expect(paneScrollPositions[scrollStateKey]).toMatchObject({
      top: 760,
      shouldStick: false,
    });
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(false);
    expect(hook.result.current.liveTailPinned).toBe(false);

    now += 16;
    const secondResidualWheel = withInputTimestamp(
      new WheelEvent("wheel", {
        bubbles: true,
        cancelable: true,
        deltaY: 40,
      }),
      now,
    );
    act(() => {
      scrollNode.dispatchEvent(secondResidualWheel);
    });
    expect(secondResidualWheel.defaultPrevented).toBe(true);
    expect(scrollNode.scrollTop).toBe(760);
    expect(hook.result.current.liveTailPinned).toBe(false);

    now += 16;
    const coalescedResidualWheel = withInputTimestamp(
      new WheelEvent("wheel", {
        bubbles: true,
        cancelable: true,
        deltaY: 200,
      }),
      now,
    );
    act(() => {
      scrollNode.dispatchEvent(coalescedResidualWheel);
    });
    expect(coalescedResidualWheel.defaultPrevented).toBe(true);
    expect(scrollNode.scrollTop).toBe(760);

    now += 16;
    const mouseNotchAfterCoalescing = withInputTimestamp(
      new WheelEvent("wheel", {
        bubbles: true,
        cancelable: true,
        deltaY: 100,
      }),
      now,
    );
    act(() => {
      scrollNode.dispatchEvent(mouseNotchAfterCoalescing);
    });
    expect(mouseNotchAfterCoalescing.defaultPrevented).toBe(true);
    expect(scrollNode.scrollTop).toBe(760);

    for (let index = 0; index < 20; index += 1) {
      now += 16;
      const continuedNoPreludeResidual = withInputTimestamp(
        new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          deltaY: 40,
        }),
        now,
      );
      act(() => {
        scrollNode.dispatchEvent(continuedNoPreludeResidual);
      });
      expect(continuedNoPreludeResidual.defaultPrevented).toBe(true);
      expect(scrollNode.scrollTop).toBe(760);
    }

    now += 49;
    const freshWheel = withInputTimestamp(
      new WheelEvent("wheel", {
        bubbles: true,
        cancelable: true,
        deltaY: 40,
      }),
      now,
    );
    act(() => {
      scrollNode.dispatchEvent(freshWheel);
    });
    expect(scrollNode.scrollTop).toBe(800);
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(true);
    expect(hook.result.current.liveTailPinned).toBe(true);

    now += 2;
    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "ArrowUp",
        metaKey: false,
        nativeEvent: { timeStamp: now } as KeyboardEvent,
        preventDefault: vi.fn(),
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    for (let index = 0; index < 24; index += 1) {
      now += 16;
      const continuedResidualWheel = withInputTimestamp(
        new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          deltaY: 40,
        }),
        now,
      );
      act(() => {
        scrollNode.dispatchEvent(continuedResidualWheel);
      });
      expect(continuedResidualWheel.defaultPrevented).toBe(true);
      expect(scrollNode.scrollTop).toBe(760);
    }
    now += 16;
    const wheelAfterFormerAbsoluteCap = withInputTimestamp(
      new WheelEvent("wheel", {
        bubbles: true,
        cancelable: false,
        deltaY: 40,
      }),
      now,
    );
    act(() => {
      scrollNode.dispatchEvent(wheelAfterFormerAbsoluteCap);
      scrollNode.scrollTop = 800;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });
    expect(wheelAfterFormerAbsoluteCap.defaultPrevented).toBe(false);
    expect(scrollNode.scrollTop).toBe(760);
    expect(hook.result.current.liveTailPinned).toBe(false);

    now += 49;
    const wheelAfterQuietBoundary = withInputTimestamp(
      new WheelEvent("wheel", {
        bubbles: true,
        cancelable: true,
        deltaY: 40,
      }),
      now,
    );
    act(() => {
      scrollNode.dispatchEvent(wheelAfterQuietBoundary);
    });
    expect(wheelAfterQuietBoundary.defaultPrevented).toBe(true);
    expect(scrollNode.scrollTop).toBe(800);
    expect(hook.result.current.liveTailPinned).toBe(true);

    now += 2;
    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "ArrowUp",
        metaKey: false,
        nativeEvent: { timeStamp: now } as KeyboardEvent,
        preventDefault: vi.fn(),
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    now += 3;
    const sameDirectionWheel = withInputTimestamp(
      new WheelEvent("wheel", {
        bubbles: true,
        cancelable: true,
        deltaY: -40,
      }),
      now,
    );
    act(() => {
      scrollNode.dispatchEvent(sameDirectionWheel);
    });
    expect(scrollNode.scrollTop).toBe(720);
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(false);
    expect(hook.result.current.liveTailPinned).toBe(false);

    now += 3;
    const downWheelAfterUpwardEscape = withInputTimestamp(
      new WheelEvent("wheel", {
        bubbles: true,
        cancelable: true,
        deltaY: 40,
      }),
      now,
    );
    act(() => {
      scrollNode.dispatchEvent(downWheelAfterUpwardEscape);
    });
    expect(downWheelAfterUpwardEscape.defaultPrevented).toBe(true);
    expect(scrollNode.scrollTop).toBe(760);
  });

  it("keeps a superseded wheel tail blocked when ArrowDown reverses repeated ArrowUp", () => {
    let now = 1_000;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState(params(session(false)));
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    act(() => {
      scrollNode.dispatchEvent(
        withInputTimestamp(
          new WheelEvent("wheel", {
            bubbles: true,
            cancelable: true,
            deltaY: 100,
          }),
          now,
        ),
      );
    });

    for (let index = 0; index < 8; index += 1) {
      now += 20;
      act(() => {
        hook.result.current.handleMessageStackUserScrollIntent({
          altKey: false,
          ctrlKey: false,
          currentTarget: scrollNode,
          defaultPrevented: false,
          key: "ArrowUp",
          metaKey: false,
          nativeEvent: { timeStamp: now } as KeyboardEvent,
          preventDefault: vi.fn(),
          shiftKey: false,
          target: scrollNode,
          type: "keydown",
        } as unknown as ReactKeyboardEvent<HTMLElement>);
      });
      expect(scrollNode.scrollTop).toBe(760 - index * 40);

      now += 5;
      const residualWheel = withInputTimestamp(
        new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          deltaY: 100,
        }),
        now,
      );
      act(() => {
        scrollNode.dispatchEvent(residualWheel);
      });
      expect(residualWheel.defaultPrevented).toBe(true);
      expect(scrollNode.scrollTop).toBe(760 - index * 40);
    }

    now += 20;
    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "ArrowDown",
        metaKey: false,
        nativeEvent: { timeStamp: now } as KeyboardEvent,
        preventDefault: vi.fn(),
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(scrollNode.scrollTop).toBe(520);

    now += 3;
    const lateResidualWheel = withInputTimestamp(
      new WheelEvent("wheel", {
        bubbles: true,
        cancelable: false,
        deltaY: 400,
      }),
      now,
    );
    act(() => {
      scrollNode.dispatchEvent(lateResidualWheel);
      scrollNode.scrollTop = 800;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
        nativeEvent: { timeStamp: now },
      } as unknown as ReactUIEvent<HTMLElement>);
    });

    expect(lateResidualWheel.defaultPrevented).toBe(false);
    expect(scrollNode.scrollTop).toBe(520);
    expect(hook.result.current.liveTailPinned).toBe(false);
    hook.unmount();
  });

  it("accepts a cancelable downward wheel reversal without a pre-key wheel prelude", () => {
    let now = 1_000;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState(params(session(false)));
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "ArrowUp",
        metaKey: false,
        preventDefault: vi.fn(),
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(scrollNode.scrollTop).toBe(760);

    now += 3;
    const deliberateDownWheel = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      deltaY: 40,
    });
    act(() => {
      scrollNode.dispatchEvent(deliberateDownWheel);
    });

    expect(deliberateDownWheel.defaultPrevented).toBe(true);
    expect(scrollNode.scrollTop).toBe(800);
    expect(hook.result.current.liveTailPinned).toBe(true);
    hook.unmount();
  });

  it("uses input timestamps when the main thread delays a residual wheel handler", () => {
    let handlerNow = 1_000;
    vi.spyOn(performance, "now").mockImplementation(() => handlerNow);
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState(params(session(false)));
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    const boundaryWheel = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      deltaY: 40,
    });
    Object.defineProperty(boundaryWheel, "timeStamp", {
      configurable: true,
      value: 1_000,
    });
    act(() => {
      scrollNode.dispatchEvent(boundaryWheel);
    });

    handlerNow = 1_002;
    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "ArrowUp",
        metaKey: false,
        nativeEvent: { timeStamp: 1_002 } as KeyboardEvent,
        preventDefault: vi.fn(),
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(scrollNode.scrollTop).toBe(760);

    handlerNow = 1_118;
    const delayedResidualWheel = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      deltaY: 40,
    });
    Object.defineProperty(delayedResidualWheel, "timeStamp", {
      configurable: true,
      value: 1_010,
    });
    act(() => {
      scrollNode.dispatchEvent(delayedResidualWheel);
    });

    expect(delayedResidualWheel.defaultPrevented).toBe(true);
    expect(scrollNode.scrollTop).toBe(760);
    expect(hook.result.current.liveTailPinned).toBe(false);

    const wheelAfterInputQuiet = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      deltaY: 40,
    });
    Object.defineProperty(wheelAfterInputQuiet, "timeStamp", {
      configurable: true,
      value: 1_060,
    });
    act(() => {
      scrollNode.dispatchEvent(wheelAfterInputQuiet);
    });

    expect(wheelAfterInputQuiet.defaultPrevented).toBe(true);
    expect(scrollNode.scrollTop).toBe(800);
    hook.unmount();
  });

  it("gives browser-owned Shift+Space authority over a confirmed residual down-wheel gesture", () => {
    let now = 1_000;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState(params(session(false)));
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    act(() => {
      scrollNode.dispatchEvent(
        withInputTimestamp(
          new WheelEvent("wheel", {
            bubbles: true,
            cancelable: true,
            deltaY: 40,
          }),
          now,
        ),
      );
      now += 2;
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: " ",
        metaKey: false,
        nativeEvent: { timeStamp: now } as KeyboardEvent,
        preventDefault: vi.fn(),
        shiftKey: true,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
      scrollNode.scrollTop = 600;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    now += 3;
    const residualDownWheel = withInputTimestamp(
      new WheelEvent("wheel", {
        bubbles: true,
        cancelable: true,
        deltaY: 40,
      }),
      now,
    );
    act(() => {
      scrollNode.dispatchEvent(residualDownWheel);
    });

    expect(residualDownWheel.defaultPrevented).toBe(true);
    expect(scrollNode.scrollTop).toBe(600);
    expect(hook.result.current.liveTailPinned).toBe(false);
    hook.unmount();
  });

  it("keeps an older wheel guard across browser-owned Space", () => {
    let now = 1_000;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState(params(session(false)));
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "ArrowUp",
        metaKey: false,
        preventDefault: vi.fn(),
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
      now += 2;
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: " ",
        metaKey: false,
        preventDefault: vi.fn(),
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(scrollNode.scrollTop).toBe(760);

    now += 3;
    const downWheel = withInputTimestamp(
      new WheelEvent("wheel", {
        bubbles: true,
        cancelable: false,
        deltaY: 40,
      }),
      now,
    );
    act(() => {
      scrollNode.dispatchEvent(downWheel);
    });

    expect(downWheel.defaultPrevented).toBe(false);
    expect(scrollNode.scrollTop).toBe(760);
    expect(hook.result.current.liveTailPinned).toBe(false);
    hook.unmount();
  });

  it.each([
    ["PageUp", (state: ReturnType<typeof useSessionPaneScrollState>) =>
      state.scrollSessionMessageStackByPageJump(-1)],
    ["Home", (state: ReturnType<typeof useSessionPaneScrollState>) =>
      state.scrollMessageStackToBoundary("top")],
  ] as const)(
    "supersedes a residual downward wheel burst with %s",
    (_key, navigate) => {
      let now = 1_000;
      vi.spyOn(performance, "now").mockImplementation(() => now);
      const scrollNode = document.createElement("section");
      Object.defineProperties(scrollNode, {
        clientHeight: { configurable: true, value: 200 },
        scrollHeight: { configurable: true, value: 1_000 },
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
      const activeSession = {
        ...session(false),
        hasOlderHistory: false,
        messagesLoaded: true,
      };
      const hook = renderHook(() => {
        const state = useSessionPaneScrollState({
          ...params(activeSession),
          isActive: true,
          isSessionTabActive: true,
        });
        useLayoutEffect(() => {
          state.messageStackRef.current = scrollNode;
        }, [state.messageStackRef]);
        return state;
      });

      const boundaryWheel = withInputTimestamp(
        new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          deltaY: 40,
        }),
        now,
      );
      act(() => {
        scrollNode.dispatchEvent(boundaryWheel);
      });
      expect(scrollNode.scrollTop).toBe(800);

      now += 2;
      act(() => {
        navigate(hook.result.current);
      });
      const topAfterNavigation = scrollNode.scrollTop;
      expect(topAfterNavigation).toBeLessThan(800);

      now += 3;
      const residualWheel = withInputTimestamp(
        new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          deltaY: 40,
        }),
        now,
      );
      act(() => {
        scrollNode.dispatchEvent(residualWheel);
      });

      expect(residualWheel.defaultPrevented).toBe(true);
      expect(scrollNode.scrollTop).toBe(topAfterNavigation);
      hook.unmount();
    },
  );

  it("rewinds only late frames from an explicitly cancelled bottom follow", () => {
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    let now = 1_000;
    vi.spyOn(performance, "now").mockImplementation(() => now);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: { top: 800, shouldStick: true },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: true },
    };
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
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState({
        ...params(session(false)),
        isSessionTabActive: true,
        paneScrollPositions,
        paneShouldStickToBottomRef,
        scrollStateKey,
      });
      state.messageStackRef.current = scrollNode;
      return state;
    });

    scrollHeight = 1_040;
    act(() => {
      expect(
        requestMessageStackBottomRepin(scrollNode, { beforePaint: true }),
      ).toBe(true);
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "ArrowUp",
        metaKey: false,
        preventDefault: vi.fn(),
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(scrollNode.scrollTop).toBe(800);

    // The canceled producer targeted "bottom", not the numeric bottom that
    // existed when it began. A later growth moves that late frame to 880.
    scrollHeight = 1_080;
    act(() => {
      scrollNode.scrollTop = 880;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });
    expect(scrollNode.scrollTop).toBe(800);
    expect(hook.result.current.liveTailPinned).toBe(false);

    now += MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS + 1;
    act(() => {
      claimMessageStackNativeScrollOwnership(
        scrollNode,
        { direction: null, owner: "pointer" },
        MESSAGE_STACK_POINTER_OWNERSHIP_MS,
      );
      scrollNode.scrollTop = 880;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });
    expect(scrollNode.scrollTop).toBe(880);
    expect(hook.result.current.liveTailPinned).toBe(true);
  });

  it("keeps a shrink detached but accepts a fractional forward movement to the physical bottom", () => {
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const activeSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: true },
    };
    let scrollHeight = 1_000;
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, get: () => scrollHeight },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const virtualizedList = document.createElement("div");
    virtualizedList.className = "virtualized-message-list";
    const visibleSlot = document.createElement("div");
    visibleSlot.className = "virtualized-message-slot";
    visibleSlot.dataset.messageId = "message-visible";
    virtualizedList.append(visibleSlot);
    scrollNode.append(virtualizedList);
    scrollNode.getBoundingClientRect = () =>
      ({ top: 100, bottom: 300 } as DOMRect);
    visibleSlot.getBoundingClientRect = () =>
      ({ top: 124, bottom: 204 } as DOMRect);
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState({
        ...params(activeSession),
        paneScrollPositions,
        paneShouldStickToBottomRef,
        scrollStateKey,
      });
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "ArrowUp",
        metaKey: false,
        preventDefault: vi.fn(),
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(scrollNode.scrollTop).toBe(760);
    expect(hook.result.current.liveTailPinned).toBe(false);

    act(() => {
      // Blink can deliver the write's native scroll event after a page
      // measurement shrinks the estimated layout. The unchanged detached
      // position is now the physical bottom, but the reader never moved down.
      scrollHeight = 960;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    expect(scrollNode.scrollTop).toBe(760);
    expect(paneScrollPositions[scrollStateKey]?.shouldStick).toBe(false);
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(false);
    expect(hook.result.current.liveTailPinned).toBe(false);

    act(() => {
      // A smaller pointer-owned clamp is still fractional geometry noise and
      // must not manufacture reader authority merely because it lands at the
      // reachable bottom.
      scrollHeight = 960.125;
      claimMessageStackNativeScrollOwnership(
        scrollNode,
        { direction: null, owner: "pointer" },
        MESSAGE_STACK_POINTER_OWNERSHIP_MS,
      );
      scrollNode.scrollTop = 760.125;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });
    expect(hook.result.current.liveTailPinned).toBe(false);

    act(() => {
      // Once geometry stabilizes, a real scrollbar-owned forward movement to
      // the physical bottom reattaches. Ownerless bottom landings remain
      // detached so a late producer cannot manufacture bottom authority.
      scrollHeight = 960.5;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
      claimMessageStackNativeScrollOwnership(
        scrollNode,
        { direction: null, owner: "pointer" },
        MESSAGE_STACK_POINTER_OWNERSHIP_MS,
      );
      scrollNode.scrollTop = 760.5;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    expect(scrollNode.scrollTop).toBe(760.5);
    expect(paneScrollPositions[scrollStateKey]?.shouldStick).toBe(true);
    expect(hook.result.current.liveTailPinned).toBe(true);
  });

  it("does not rewind a virtualizer-owned anchor correction at the detached physical bottom", () => {
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const activeSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: true },
    };
    let scrollHeight = 1_000;
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, get: () => scrollHeight },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState({
        ...params(activeSession),
        paneScrollPositions,
        paneShouldStickToBottomRef,
        scrollStateKey,
      });
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "ArrowUp",
        metaKey: false,
        preventDefault: vi.fn(),
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(scrollNode.scrollTop).toBe(760);

    act(() => {
      // First reproduce the measurement shrink that leaves the detached reader
      // at the physical bottom without granting bottom-follow authority.
      scrollHeight = 960;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    act(() => {
      // A later page-above measurement grows by 40px. The virtualizer preserves
      // the visible message anchor by applying the same +40px correction before
      // its native scroll event reaches the pane. This is layout ownership, not
      // a stale smooth-bottom tick, so the pane must retain the corrected top.
      scrollHeight = 1_000;
      markMessageStackVirtualizerPositionCorrection(scrollNode, 800);
      scrollNode.scrollTop = 800;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    expect(scrollNode.scrollTop).toBe(800);
    expect(paneScrollPositions[scrollStateKey]).toMatchObject({
      top: 800,
      shouldStick: false,
    });
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(false);
    expect(hook.result.current.liveTailPinned).toBe(false);

    act(() => {
      // The correction marker is single-use. A later unowned positive move to
      // the new physical bottom still cannot manufacture reader authority.
      // Every natural reentry path now carries an input lease.
      scrollHeight = 1_040;
      scrollNode.scrollTop = 840;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    expect(scrollNode.scrollTop).toBe(840);
    expect(paneScrollPositions[scrollStateKey]).toMatchObject({
      top: 840,
      shouldStick: false,
    });
    expect(hook.result.current.liveTailPinned).toBe(false);
  });

  it("reattaches a Home-detached pane after one native thumb move to bottom", () => {
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const activeSession = {
      ...session(false),
      hasOlderHistory: false,
      messagesLoaded: true,
    };
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: true },
    };
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    scrollNode.scrollTo = vi.fn(
      (optionsOrX?: ScrollToOptions | number, y?: number) => {
        scrollNode.scrollTop =
          typeof optionsOrX === "number"
            ? (y ?? scrollNode.scrollTop)
            : (optionsOrX?.top ?? scrollNode.scrollTop);
      },
    ) as typeof scrollNode.scrollTo;
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState({
        ...params(activeSession),
        paneScrollPositions,
        paneShouldStickToBottomRef,
        scrollStateKey,
      });
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    act(() => {
      hook.result.current.scrollMessageStackToBoundary("top");
    });
    expect(scrollNode.scrollTop).toBe(0);
    expect(hook.result.current.liveTailPinned).toBe(false);

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        currentTarget: scrollNode,
        defaultPrevented: false,
        target: scrollNode,
        type: "mousedown",
      } as unknown as ReactMouseEvent<HTMLElement>);
      scrollNode.scrollTop = 800;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    expect(paneScrollPositions[scrollStateKey]?.shouldStick).toBe(true);
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(true);
    expect(hook.result.current.liveTailPinned).toBe(true);
  });

  it.each(["touch inertia", "focus scrollIntoView", "browser Space"] as const)(
    "reattaches a detached pane when %s reaches the physical bottom",
    (ownerKind) => {
      vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
      vi.stubGlobal("cancelAnimationFrame", vi.fn());
      let now = 1_000;
      vi.spyOn(performance, "now").mockImplementation(() => now);
      const activeSession = {
        ...session(false),
        hasOlderHistory: false,
        messagesLoaded: true,
      };
      const scrollStateKey = "pane-1:session-history";
      const paneScrollPositions = {
        [scrollStateKey]: { top: Number.MAX_SAFE_INTEGER, shouldStick: true },
      };
      const paneShouldStickToBottomRef = {
        current: { [scrollStateKey]: true },
      };
      const scrollNode = document.createElement("section");
      const focusedButton = document.createElement("button");
      scrollNode.append(focusedButton);
      scrollNode.getBoundingClientRect = () =>
        ({ top: 0, bottom: 200 } as DOMRect);
      focusedButton.getBoundingClientRect = () =>
        ({ top: 240, bottom: 272 } as DOMRect);
      Object.defineProperties(scrollNode, {
        clientHeight: { configurable: true, value: 200 },
        scrollHeight: { configurable: true, value: 1_000 },
        scrollTop: { configurable: true, writable: true, value: 800 },
      });
      scrollNode.scrollTo = vi.fn(
        (optionsOrX?: ScrollToOptions | number, y?: number) => {
          scrollNode.scrollTop =
            typeof optionsOrX === "number"
              ? (y ?? scrollNode.scrollTop)
              : (optionsOrX?.top ?? scrollNode.scrollTop);
        },
      ) as typeof scrollNode.scrollTo;
      const hook = renderHook(() => {
        const state = useSessionPaneScrollState({
          ...params(activeSession),
          paneScrollPositions,
          paneShouldStickToBottomRef,
          scrollStateKey,
        });
        useLayoutEffect(() => {
          state.messageStackRef.current = scrollNode;
        }, [state.messageStackRef]);
        return state;
      });

      act(() => {
        hook.result.current.scrollMessageStackToBoundary("top");
        if (ownerKind === "touch inertia") {
          hook.result.current.handleMessageStackTouchStart({
            currentTarget: scrollNode,
            touches: [{ clientY: 100 }],
          } as unknown as ReactTouchEvent<HTMLElement>);
          hook.result.current.handleMessageStackUserScrollIntent({
            currentTarget: scrollNode,
            target: scrollNode,
            touches: [{ clientY: 80 }],
            type: "touchmove",
          } as unknown as ReactTouchEvent<HTMLElement>);
        } else if (ownerKind === "focus scrollIntoView") {
          hook.result.current.handleMessageStackFocusCapture({
            currentTarget: scrollNode,
            target: focusedButton,
          } as unknown as ReactFocusEvent<HTMLElement>);
        } else {
          hook.result.current.handleMessageStackUserScrollIntent({
            altKey: false,
            ctrlKey: false,
            currentTarget: scrollNode,
            defaultPrevented: false,
            key: " ",
            metaKey: false,
            preventDefault: vi.fn(),
            shiftKey: false,
            target: scrollNode,
            type: "keydown",
          } as unknown as ReactKeyboardEvent<HTMLElement>);
          now += 500;
        }
        scrollNode.scrollTop = 770;
        hook.result.current.handleMessageStackScroll({
          currentTarget: scrollNode,
        } as ReactUIEvent<HTMLElement>);
      });

      expect(paneScrollPositions[scrollStateKey]?.shouldStick).toBe(false);
      expect(hook.result.current.liveTailPinned).toBe(false);

      act(() => {
        scrollNode.scrollTop = 800;
        hook.result.current.handleMessageStackScroll({
          currentTarget: scrollNode,
        } as ReactUIEvent<HTMLElement>);
      });

      expect(paneScrollPositions[scrollStateKey]?.shouldStick).toBe(true);
      expect(hook.result.current.liveTailPinned).toBe(true);
      hook.unmount();
    },
  );

  it("releases shared pointer ownership when the window loses focus", () => {
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 400 },
    });
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState({
        ...params(session(false)),
        isActive: true,
        isSessionTabActive: true,
      });
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        currentTarget: scrollNode,
        defaultPrevented: false,
        target: scrollNode,
        type: "mousedown",
      } as unknown as ReactMouseEvent<HTMLElement>);
    });
    expect(peekMessageStackNativeScrollOwnership(scrollNode)?.owner).toBe(
      "pointer",
    );

    act(() => {
      window.dispatchEvent(new Event("blur"));
    });
    expect(peekMessageStackNativeScrollOwnership(scrollNode)).toBeNull();
    hook.unmount();
  });

  it("keeps a downward wheel move detached inside the near-bottom band", () => {
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const activeSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: { top: 720, shouldStick: true },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: true },
    };
    const scrollNode = document.createElement("section");
    const virtualizedList = document.createElement("div");
    virtualizedList.className = "virtualized-message-list";
    const visibleSlot = document.createElement("div");
    visibleSlot.className = "virtualized-message-slot";
    visibleSlot.dataset.messageId = "message-visible";
    virtualizedList.append(visibleSlot);
    scrollNode.append(virtualizedList);
    scrollNode.getBoundingClientRect = () =>
      ({ top: 100, bottom: 300 } as DOMRect);
    visibleSlot.getBoundingClientRect = () => {
      const top = 124 - (scrollNode.scrollTop - 720);
      return { top, bottom: top + 80 } as DOMRect;
    };
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 720 },
    });
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState({
        ...params(activeSession),
        paneScrollPositions,
        paneShouldStickToBottomRef,
        scrollStateKey,
      });
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    const wheelEvent = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      deltaY: 40,
    });
    act(() => {
      scrollNode.dispatchEvent(wheelEvent);
    });

    expect(wheelEvent.defaultPrevented).toBe(true);
    expect(scrollNode.scrollTop).toBe(760);
    expect(paneScrollPositions[scrollStateKey]).toEqual({
      anchor: {
        messageId: "message-visible",
        viewportOffsetPx: -16,
      },
      top: 760,
      shouldStick: false,
    });
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(false);
    expect(hook.result.current.liveTailPinned).toBe(false);
  });

  it("restores attached presentation when a downward page command reaches the physical bottom", () => {
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const activeSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: { top: 740, shouldStick: true },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: true },
    };
    const scrollNode = document.createElement("section");
    const conversationPage = document.createElement("div");
    conversationPage.className = "session-conversation-page is-active";
    const liveTail = document.createElement("div");
    liveTail.className = "conversation-live-tail";
    liveTail.setAttribute("data-tail-follow", "attached");
    conversationPage.append(liveTail);
    scrollNode.append(conversationPage);
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 740 },
    });
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState({
        ...params(activeSession),
        paneScrollPositions,
        paneShouldStickToBottomRef,
        scrollStateKey,
      });
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    act(() => {
      hook.result.current.scrollMessageStackByPage(1);
    });

    expect(scrollNode.scrollTop).toBe(800);
    expect(liveTail).toHaveAttribute("data-tail-follow", "attached");
    expect(paneScrollPositions[scrollStateKey]).toEqual({
      top: 800,
      shouldStick: true,
    });
    expect(hook.result.current.liveTailPinned).toBe(true);
  });

  it("keeps attached presentation when a downward wheel lands inside the fractional physical-bottom tolerance", () => {
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const activeSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: { top: 790.5, shouldStick: true },
    };
    const paneShouldStickToBottomRef = {
      current: { [scrollStateKey]: true },
    };
    const scrollNode = document.createElement("section");
    const conversationPage = document.createElement("div");
    conversationPage.className = "session-conversation-page is-active";
    const liveTail = document.createElement("div");
    liveTail.className = "conversation-live-tail";
    liveTail.setAttribute("data-tail-follow", "attached");
    conversationPage.append(liveTail);
    scrollNode.append(conversationPage);
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 790.5 },
    });
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState({
        ...params(activeSession),
        paneScrollPositions,
        paneShouldStickToBottomRef,
        scrollStateKey,
      });
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    const wheelEvent = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      deltaY: 6,
    });
    act(() => {
      scrollNode.dispatchEvent(wheelEvent);
    });

    expect(wheelEvent.defaultPrevented).toBe(true);
    expect(scrollNode.scrollTop).toBe(796.5);
    expect(liveTail).toHaveAttribute("data-tail-follow", "attached");
    expect(paneScrollPositions[scrollStateKey]).toEqual({
      top: 796.5,
      shouldStick: true,
    });
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(true);
    expect(hook.result.current.liveTailPinned).toBe(true);
  });

  it("keeps control-owned keys attached but detaches unhandled link scroll keys", () => {
    const activeSession = session(false);
    const scrollNode = document.createElement("section");
    const button = document.createElement("button");
    const textarea = document.createElement("textarea");
    const anchor = document.createElement("a");
    anchor.href = "#docs";
    scrollNode.append(button, textarea, anchor);
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const sharedParams = {
      ...params(activeSession),
      paneShouldStickToBottomRef: {
        current: { "pane-1:session-history": true },
      },
    };
    const hook = renderHook(() => useSessionPaneScrollState(sharedParams));
    hook.result.current.messageStackRef.current = scrollNode;

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: " ",
        metaKey: false,
        shiftKey: false,
        target: textarea,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(hook.result.current.liveTailPinned).toBe(true);

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: " ",
        metaKey: false,
        shiftKey: false,
        target: button,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(hook.result.current.liveTailPinned).toBe(true);

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: true,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "PageDown",
        metaKey: false,
        shiftKey: false,
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(hook.result.current.liveTailPinned).toBe(true);

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "ArrowUp",
        metaKey: false,
        preventDefault: vi.fn(),
        shiftKey: false,
        target: anchor,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(hook.result.current.liveTailPinned).toBe(false);
  });

  it("keeps direct transcript keys with a nested scroller that can move", () => {
    const activeSession = session(false);
    const scrollNode = document.createElement("section");
    const nestedScroller = document.createElement("div");
    const nestedContent = document.createElement("span");
    nestedScroller.tabIndex = 0;
    nestedScroller.style.overflowY = "auto";
    nestedScroller.append(nestedContent);
    scrollNode.append(nestedScroller);
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    Object.defineProperties(nestedScroller, {
      clientHeight: { configurable: true, value: 100 },
      scrollHeight: { configurable: true, value: 400 },
      scrollTop: { configurable: true, writable: true, value: 100 },
    });
    const hook = renderHook(() => useSessionPaneScrollState(params(activeSession)));
    hook.result.current.messageStackRef.current = scrollNode;
    const intentListener = vi.fn();
    scrollNode.addEventListener(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      intentListener,
    );

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "PageUp",
        metaKey: false,
        shiftKey: false,
        target: nestedContent,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });

    expect(intentListener).not.toHaveBeenCalled();
    expect(hook.result.current.liveTailPinned).toBe(true);
  });

  it("uses stack semantics for PageDown from a transcript text input", () => {
    const activeSession = session(false);
    const scrollNode = document.createElement("section");
    const textInput = document.createElement("input");
    textInput.type = "text";
    textInput.value = "selection text";
    textInput.setSelectionRange(4, 4);
    scrollNode.append(textInput);
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 500 },
    });
    const hook = renderHook(() => useSessionPaneScrollState(params(activeSession)));
    hook.result.current.messageStackRef.current = scrollNode;
    const intentListener = vi.fn();
    scrollNode.addEventListener(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      intentListener,
    );
    const nativeEvent = new KeyboardEvent("keydown", {
      bubbles: true,
      key: "PageDown",
    });

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        altKey: false,
        ctrlKey: false,
        currentTarget: scrollNode,
        defaultPrevented: false,
        key: "PageDown",
        metaKey: false,
        nativeEvent,
        shiftKey: false,
        target: textInput,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });

    expect(intentListener).toHaveBeenCalledTimes(1);
    expect(hook.result.current.liveTailPinned).toBe(true);
  });

  it("reads a mutable saved tail intent at callback time", () => {
    const activeSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: { top: 800, shouldStick: true },
    };
    const paneShouldStickToBottomRef = {
      current: {} as Record<string, boolean>,
    };
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 300 },
    });
    const hook = renderHook(() =>
      useSessionPaneScrollState({
        ...params(activeSession),
        paneScrollPositions,
        paneShouldStickToBottomRef,
        scrollStateKey,
      }),
    );
    hook.result.current.messageStackRef.current = scrollNode;

    // Mutable pane geometry can change after render but before an rAF/native
    // callback. The callback must not revive the captured attached snapshot.
    paneScrollPositions[scrollStateKey] = {
      top: 300,
      shouldStick: false,
    };
    act(() => {
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    expect(paneScrollPositions[scrollStateKey]).toEqual({
      top: 300,
      shouldStick: false,
    });
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(false);
  });

  it("cancels active message reveals for touch and PageUp navigation", () => {
    const activeSession = session(false);
    const scrollNode = document.createElement("section");
    const revealShell = document.createElement("article");
    scrollNode.append(revealShell);
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 500 },
    });
    const hook = renderHook(() =>
      useSessionPaneScrollState({
        ...params(activeSession),
        isActive: true,
        isSessionTabActive: true,
      }),
    );
    hook.result.current.messageStackRef.current = scrollNode;
    const armReveal = () => {
      revealShell.removeAttribute(
        "data-conversation-message-entry-reveal-cancelled",
      );
      revealShell.classList.add("conversation-message-entry-reveal");
    };

    armReveal();
    act(() => {
      hook.result.current.handleMessageStackTouchStart({
        touches: [{ clientY: 100 }],
      } as unknown as ReactTouchEvent<HTMLElement>);
      hook.result.current.handleMessageStackUserScrollIntent({
        currentTarget: scrollNode,
        target: scrollNode,
        touches: [{ clientY: 140 }],
        type: "touchmove",
      } as unknown as ReactTouchEvent<HTMLElement>);
    });
    expect(revealShell).not.toHaveClass(
      "conversation-message-entry-reveal",
    );

    armReveal();
    act(() => {
      hook.result.current.scrollSessionMessageStackByPageJump(-1);
    });
    expect(revealShell).not.toHaveClass(
      "conversation-message-entry-reveal",
    );
    expect(revealShell).toHaveAttribute(
      "data-conversation-message-entry-reveal-cancelled",
    );
  });

  it("does not advertise phantom newer content while explicitly tail-pinned", () => {
    expect(
      resolveNewResponseIndicatorVisibility({
        hasUnloadedNewerHistory: false,
        indicatorKind: "response",
        liveTailPinned: true,
      }),
    ).toBe(false);
    expect(
      resolveNewResponseIndicatorVisibility({
        hasUnloadedNewerHistory: true,
        indicatorKind: null,
        liveTailPinned: false,
      }),
    ).toBe(true);
  });

  it("does not replay a stale detached position after tail-follow re-enters", () => {
    const animationFrames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", ((
      callback: FrameRequestCallback,
    ) => {
      animationFrames.push(callback);
      return animationFrames.length;
    }) as typeof requestAnimationFrame);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const liveSession = session(false);
    const scrollStateKey = "pane-1:session-history";
    const paneScrollPositions = {
      [scrollStateKey]: {
        top: 51_966,
        shouldStick: false,
      },
    };
    const paneShouldStickToBottomRef = {
      current: {
        [scrollStateKey]: true,
      },
    };
    const sharedParams = {
      ...params(liveSession),
      paneScrollPositions,
      paneShouldStickToBottomRef,
    };
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 567 },
      scrollHeight: { configurable: true, value: 53_355 },
      scrollTop: { configurable: true, writable: true, value: 52_788 },
    });
    const hook = renderHook(
      ({ isSessionTabActive, activeScrollStateKey }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          isSessionTabActive,
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

    expect(scrollNode.scrollTop).toBe(52_788);
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(true);
    expect(animationFrames.length).toBeGreaterThan(0);
  });

  it("restores an attached tab to the bottom before its first animation frame", () => {
    const animationFrames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", ((
      callback: FrameRequestCallback,
    ) => {
      animationFrames.push(callback);
      return animationFrames.length;
    }) as typeof requestAnimationFrame);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const liveSession = session(false);
    const sessionScrollKey = "pane-1:session-history";
    const paneScrollPositions = {
      [sessionScrollKey]: {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      },
    };
    const sharedParams = {
      ...params(liveSession),
      isActive: true,
      paneScrollPositions,
      paneShouldStickToBottomRef: { current: { [sessionScrollKey]: true } },
    };
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 240 },
    });
    const scrollWrites: CustomEvent[] = [];
    scrollNode.addEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, (event) => {
      scrollWrites.push(event as CustomEvent);
    });
    const hook = renderHook(
      ({ isSessionTabActive, scrollStateKey }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          isSessionTabActive,
          scrollStateKey,
        }),
      {
        initialProps: {
          isSessionTabActive: false,
          scrollStateKey: "pane-1:control-panel",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;

    hook.rerender({
      isSessionTabActive: true,
      scrollStateKey: sessionScrollKey,
    });

    expect(scrollNode.scrollTop).toBe(800);
    expect(scrollWrites[scrollWrites.length - 1]?.detail).toMatchObject({
      scrollKind: "bottom_pin",
    });
    expect(animationFrames.length).toBeGreaterThan(0);
  });

  it("does not let first-visit follow overwrite a restored detached tab", () => {
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
    const attachedSession = {
      ...session(false),
      id: "session-attached",
    };
    const detachedSession = {
      ...session(false),
      id: "session-detached",
    };
    const attachedKey = "pane-1:session-attached";
    const detachedKey = "pane-1:session-detached";
    const paneScrollPositions = {
      [attachedKey]: {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      },
      [detachedKey]: {
        top: 320,
        shouldStick: false,
      },
    };
    const paneShouldStickToBottomRef = {
      current: {
        "pane-1": true,
        [attachedKey]: true,
        [detachedKey]: false,
      },
    };
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
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
    const sharedParams = {
      ...params(attachedSession),
      isActive: true,
      paneScrollPositions,
      paneShouldStickToBottomRef,
    };
    const hook = renderHook(
      ({ activeSession, isSessionTabActive, scrollStateKey }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession,
          isSessionTabActive,
          scrollStateKey,
        }),
      {
        initialProps: {
          activeSession: attachedSession,
          isSessionTabActive: false,
          scrollStateKey: attachedKey,
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;

    hook.rerender({
      activeSession: detachedSession,
      isSessionTabActive: true,
      scrollStateKey: detachedKey,
    });

    expect(scrollNode.scrollTop).toBe(320);
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

    expect(scrollNode.scrollTop).toBe(320);
    expect(paneScrollPositions[detachedKey]).toEqual({
      top: 320,
      shouldStick: false,
    });
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

  it("retains an out-of-range detached target until virtualized geometry catches up", () => {
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
    const scrollStateKey = "pane-1:session-detached";
    const paneScrollPositions = {
      [scrollStateKey]: {
        top: 600,
        shouldStick: false,
      },
    };
    const paneShouldStickToBottomRef = {
      current: {
        [scrollStateKey]: false,
      },
    };
    let scrollHeight = 600;
    const scrollNode = document.createElement("section");
    scrollNode.append(
      Object.assign(document.createElement("div"), {
        className: "virtualized-message-list",
      }),
    );
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, get: () => scrollHeight },
      scrollTop: { configurable: true, writable: true, value: 120 },
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

    expect(paneScrollPositions[scrollStateKey]).toEqual({
      top: 600,
      shouldStick: false,
    });
    markMessageStackVirtualizerPositionCorrection(
      scrollNode,
      scrollNode.scrollTop,
    );
    act(() => {
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });
    expect(consumeMessageStackVirtualizerPositionCorrection(scrollNode)).toBe(
      false,
    );
    expect(paneScrollPositions[scrollStateKey]).toEqual({
      top: 600,
      shouldStick: false,
    });
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
    act(() => {
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });
    expect(paneScrollPositions[scrollStateKey]).toEqual({
      top: 600,
      shouldStick: false,
    });
    expect(paneShouldStickToBottomRef.current[scrollStateKey]).toBe(false);

    // The matching native event ends restore ownership. A later virtualizer
    // anchor correction must publish its adjusted detached position instead
    // of replaying the old absolute restore target forever.
    scrollNode.scrollTop = 625;
    act(() => {
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });
    expect(paneScrollPositions[scrollStateKey]).toEqual({
      top: 625,
      shouldStick: false,
    });
  });

  it("publishes the reachable detached position when restore convergence expires", () => {
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
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 600 },
      scrollTop: { configurable: true, writable: true, value: 120 },
    });
    const hook = renderHook(
      ({ isSessionTabActive, scrollStateKey }) =>
        useSessionPaneScrollState({
          ...params(liveSession),
          isActive: true,
          isSessionTabActive,
          paneScrollPositions,
          paneShouldStickToBottomRef,
          scrollStateKey,
        }),
      {
        initialProps: {
          isSessionTabActive: false,
          scrollStateKey: "pane-1:control-panel",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;
    hook.rerender({
      isSessionTabActive: true,
      scrollStateKey: detachedKey,
    });

    let frameTimestamp = 0;
    let drainedFrames = 0;
    while (animationFrames.size > 0 && drainedFrames < 65) {
      const nextFrame = animationFrames.entries().next().value;
      if (!nextFrame) {
        break;
      }
      animationFrames.delete(nextFrame[0]);
      frameTimestamp += 1000 / 60;
      act(() => nextFrame[1](frameTimestamp));
      drainedFrames += 1;
    }

    expect(drainedFrames).toBe(60);
    expect(scrollNode.scrollTop).toBe(400);
    expect(paneScrollPositions[detachedKey]).toEqual({
      top: 400,
      shouldStick: false,
    });
    expect(paneShouldStickToBottomRef.current[detachedKey]).toBe(false);
    expect(animationFrames.size).toBe(0);
  });

  it("gives an explicit bottom jump authority before virtualizer reconciliation", () => {
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
    const scrollNode = document.createElement("section");
    scrollNode.append(
      Object.assign(document.createElement("div"), {
        className: "virtualized-message-list",
      }),
    );
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 600 },
      scrollTop: { configurable: true, writable: true, value: 120 },
    });
    const hook = renderHook(
      ({ isSessionTabActive, scrollStateKey }) =>
        useSessionPaneScrollState({
          ...params(liveSession),
          isActive: true,
          isSessionTabActive,
          paneScrollPositions,
          paneShouldStickToBottomRef,
          scrollStateKey,
        }),
      {
        initialProps: {
          isSessionTabActive: false,
          scrollStateKey: "pane-1:control-panel",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;
    let positionObservedDuringBoundaryWrite:
      | { top: number; shouldStick: boolean }
      | undefined;
    scrollNode.addEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, (event) => {
      if (
        (event as CustomEvent).detail?.scrollKind !== "bottom_boundary"
      ) {
        return;
      }
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
      positionObservedDuringBoundaryWrite = {
        ...paneScrollPositions[detachedKey],
      };
    });

    hook.rerender({
      isSessionTabActive: true,
      scrollStateKey: detachedKey,
    });
    expect(scrollNode.scrollTop).toBe(400);
    expect(animationFrames.size).toBeGreaterThan(0);

    act(() => {
      hook.result.current.scrollMessageStackToBoundary("bottom");
    });

    expect(positionObservedDuringBoundaryWrite).toEqual({
      top: 400,
      shouldStick: true,
    });
    expect(paneScrollPositions[detachedKey]).toEqual({
      top: Number.MAX_SAFE_INTEGER,
      shouldStick: true,
    });
    expect(paneShouldStickToBottomRef.current[detachedKey]).toBe(true);
    expect(animationFrames.size).toBe(0);
  });

  it("does not let a retained restore frame revert a button-driven page jump", () => {
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
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_200 },
      scrollTop: { configurable: true, writable: true, value: 200 },
    });
    const hook = renderHook(
      ({ isSessionTabActive, scrollStateKey }) =>
        useSessionPaneScrollState({
          ...params(liveSession),
          isActive: true,
          isSessionTabActive,
          paneScrollPositions,
          paneShouldStickToBottomRef: {
            current: { [detachedKey]: false },
          },
          scrollStateKey,
        }),
      {
        initialProps: {
          isSessionTabActive: false,
          scrollStateKey: "pane-1:control-panel",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;
    hook.rerender({
      isSessionTabActive: true,
      scrollStateKey: detachedKey,
    });
    expect(scrollNode.scrollTop).toBe(600);
    const retainedRestoreFrame = animationFrames.values().next().value;
    if (!retainedRestoreFrame) {
      throw new Error("Detached restore verification frame was not scheduled");
    }

    act(() => {
      hook.result.current.scrollSessionMessageStackByPageJump(1);
    });
    const pageJumpTop = scrollNode.scrollTop;
    expect(pageJumpTop).toBeGreaterThan(600);

    // cancelAnimationFrame cannot stop a callback the browser already handed
    // off. The controller's cancelled guard must still reject that stale tick.
    act(() => retainedRestoreFrame(1000 / 60));
    expect(scrollNode.scrollTop).toBe(pageJumpTop);
    expect(paneScrollPositions[detachedKey]).toEqual({
      top: pageJumpTop,
      shouldStick: false,
    });
  });

  it("does not let a retained restore frame revert scrollbar-thumb navigation", () => {
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
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_200 },
      scrollTop: { configurable: true, writable: true, value: 200 },
    });
    const hook = renderHook(
      ({ isSessionTabActive, scrollStateKey }) =>
        useSessionPaneScrollState({
          ...params(liveSession),
          isActive: true,
          isSessionTabActive,
          paneScrollPositions,
          paneShouldStickToBottomRef: {
            current: { [detachedKey]: false },
          },
          scrollStateKey,
        }),
      {
        initialProps: {
          isSessionTabActive: false,
          scrollStateKey: "pane-1:control-panel",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;
    hook.rerender({
      isSessionTabActive: true,
      scrollStateKey: detachedKey,
    });
    const retainedRestoreFrame = animationFrames.values().next().value;
    if (!retainedRestoreFrame) {
      throw new Error("Detached restore verification frame was not scheduled");
    }

    act(() => {
      hook.result.current.handleMessageStackUserScrollIntent({
        currentTarget: scrollNode,
        target: scrollNode,
        type: "mousedown",
      } as unknown as ReactMouseEvent<HTMLElement>);
      scrollNode.scrollTop = 350;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    // A callback already dequeued by the browser must still observe that the
    // scrollbar gesture transferred authority to the user.
    act(() => retainedRestoreFrame(1000 / 60));
    expect(scrollNode.scrollTop).toBe(350);
    expect(paneScrollPositions[detachedKey]).toEqual({
      top: 350,
      shouldStick: false,
    });
  });

  it("does not claim focus ownership for a control already inside the viewport", () => {
    const scrollNode = document.createElement("section");
    const focusedButton = document.createElement("button");
    scrollNode.append(focusedButton);
    scrollNode.getBoundingClientRect = () =>
      ({ top: 0, bottom: 200 } as DOMRect);
    focusedButton.getBoundingClientRect = () =>
      ({ top: 40, bottom: 72 } as DOMRect);
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_200 },
      scrollTop: { configurable: true, writable: true, value: 200 },
    });
    const hook = renderHook(() => {
      const state = useSessionPaneScrollState({
        ...params(session(false)),
        isActive: true,
        isSessionTabActive: true,
      });
      useLayoutEffect(() => {
        state.messageStackRef.current = scrollNode;
      }, [state.messageStackRef]);
      return state;
    });

    act(() => {
      hook.result.current.handleMessageStackFocusCapture({
        currentTarget: scrollNode,
        target: focusedButton,
      } as unknown as ReactFocusEvent<HTMLElement>);
    });

    expect(peekMessageStackNativeScrollOwnership(scrollNode)).toBeNull();
    hook.unmount();
  });

  it("does not let a retained restore frame revert focus navigation", () => {
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
    const scrollNode = document.createElement("section");
    const focusedButton = document.createElement("button");
    scrollNode.append(focusedButton);
    scrollNode.getBoundingClientRect = () =>
      ({ top: 0, bottom: 200 } as DOMRect);
    focusedButton.getBoundingClientRect = () =>
      ({ top: 260, bottom: 300 } as DOMRect);
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_200 },
      scrollTop: { configurable: true, writable: true, value: 200 },
    });
    const hook = renderHook(
      ({ isSessionTabActive, scrollStateKey }) =>
        useSessionPaneScrollState({
          ...params(liveSession),
          isActive: true,
          isSessionTabActive,
          paneScrollPositions,
          paneShouldStickToBottomRef: {
            current: { [detachedKey]: false },
          },
          scrollStateKey,
        }),
      {
        initialProps: {
          isSessionTabActive: false,
          scrollStateKey: "pane-1:control-panel",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;
    hook.rerender({
      isSessionTabActive: true,
      scrollStateKey: detachedKey,
    });
    const retainedRestoreFrame = animationFrames.values().next().value;
    if (!retainedRestoreFrame) {
      throw new Error("Detached restore verification frame was not scheduled");
    }

    act(() => {
      hook.result.current.handleMessageStackFocusCapture({
        currentTarget: scrollNode,
        target: focusedButton,
      } as unknown as ReactFocusEvent<HTMLElement>);
      scrollNode.scrollTop = 350;
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });

    // Browser focus may call scrollIntoView without a wheel/key precursor. It
    // still transfers authority away from the pending detached restoration.
    act(() => retainedRestoreFrame(1000 / 60));
    expect(scrollNode.scrollTop).toBe(350);
    expect(paneScrollPositions[detachedKey]).toEqual({
      top: 350,
      shouldStick: false,
    });
  });

  it("rechecks a zero-write detached restore after the virtualized range commit", () => {
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
    let scrollHeight = 900;
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, get: () => scrollHeight },
      scrollTop: { configurable: true, writable: true, value: 600 },
    });
    const hook = renderHook(
      ({ isSessionTabActive, scrollStateKey }) =>
        useSessionPaneScrollState({
          ...params(liveSession),
          isActive: true,
          isSessionTabActive,
          paneScrollPositions,
          paneShouldStickToBottomRef: {
            current: { [detachedKey]: false },
          },
          scrollStateKey,
        }),
      {
        initialProps: {
          isSessionTabActive: false,
          scrollStateKey: "pane-1:control-panel",
        },
      },
    );
    hook.result.current.messageStackRef.current = scrollNode;
    hook.rerender({
      isSessionTabActive: true,
      scrollStateKey: detachedKey,
    });

    // The outgoing tab already used the same numeric top, so the synchronous
    // restore performs no DOM write. The incoming virtualized commit then
    // shrinks and clamps the range before the verification frame.
    expect(scrollNode.scrollTop).toBe(600);
    scrollHeight = 600;
    scrollNode.scrollTop = 400;
    act(() => {
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });
    expect(paneScrollPositions[detachedKey]).toEqual({
      top: 600,
      shouldStick: false,
    });

    const firstFrame = animationFrames.entries().next().value;
    if (!firstFrame) {
      throw new Error("Detached verification frame was not scheduled");
    }
    animationFrames.delete(firstFrame[0]);
    act(() => firstFrame[1](1000 / 60));
    expect(scrollNode.scrollTop).toBe(400);
    expect(animationFrames.size).toBeGreaterThan(0);

    scrollHeight = 900;
    const secondFrame = animationFrames.entries().next().value;
    if (!secondFrame) {
      throw new Error("Detached retry frame was not scheduled");
    }
    animationFrames.delete(secondFrame[0]);
    act(() => secondFrame[1](2000 / 60));
    expect(scrollNode.scrollTop).toBe(600);
    act(() => {
      hook.result.current.handleMessageStackScroll({
        currentTarget: scrollNode,
      } as ReactUIEvent<HTMLElement>);
    });
    expect(paneScrollPositions[detachedKey]).toEqual({
      top: 600,
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

  it("does not infer a prompt send when start-page residency ends with a user message", () => {
    const requestAnimationFrame = vi.fn(() => 1);
    vi.stubGlobal("requestAnimationFrame", requestAnimationFrame);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const demands: SessionHistoryPageDemand[] = [];
    const removeListener = addSessionHistoryPageDemandListener((demand) => {
      demands.push(demand);
    });
    const historicalSession = session(true);
    const sharedParams = {
      ...params(historicalSession),
      isSessionTabActive: true,
      paneShouldStickToBottomRef: {
        current: { "pane-1:session-history": true },
      },
    };
    const hook = renderHook(
      ({ activeSession, contentSignature, lastAuthor }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession,
          visibleContentSignature: contentSignature,
          visibleLastMessageAuthor: lastAuthor,
          visibleMessageContentSignature: contentSignature,
        }),
      {
        initialProps: {
          activeSession: historicalSession,
          contentSignature: "historical-assistant-window",
          lastAuthor: "assistant" as Message["author"],
        },
      },
    );
    const animationFrameCountBeforeResidencyReplacement =
      requestAnimationFrame.mock.calls.length;

    hook.rerender({
      activeSession: {
        ...historicalSession,
        messages: [
          {
            id: "message-64",
            type: "text",
            timestamp: "12:00",
            author: "you",
            text: "Opening prompt",
          },
        ],
      },
      contentSignature: "start-page-ending-in-user-message",
      lastAuthor: "you",
    });

    expect(demands).toHaveLength(0);
    expect(requestAnimationFrame).toHaveBeenCalledTimes(
      animationFrameCountBeforeResidencyReplacement,
    );
    expect(hook.result.current.liveTailPinned).toBe(false);
    expect(hook.result.current.showNewResponseIndicator).toBe(true);
    expect(hook.result.current.newResponseIndicatorLabel).toBe(
      "Jump to latest",
    );

    removeListener();
  });

  it("keeps a historical window detached when a send starts", () => {
    const animationFrames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", ((
      callback: FrameRequestCallback,
    ) => {
      animationFrames.push(callback);
      return animationFrames.length;
    }) as typeof requestAnimationFrame);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const demands: SessionHistoryPageDemand[] = [];
    const removeListener = addSessionHistoryPageDemandListener((demand) => {
      demands.push(demand);
    });
    const historicalSession = session(true);
    const sharedParams = {
      ...params(historicalSession),
      isSessionTabActive: true,
      paneShouldStickToBottomRef: {
        current: { "pane-1:session-history": true },
      },
    };
    const hook = renderHook(
      ({ isSending }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          isSending,
        }),
      { initialProps: { isSending: false } },
    );

    animationFrames.length = 0;
    hook.rerender({ isSending: true });
    expect(demands).toHaveLength(0);
    expect(animationFrames).toHaveLength(0);

    expect(demands).toHaveLength(0);
    expect(hook.result.current.liveTailPinned).toBe(false);
    expect(hook.result.current.showNewResponseIndicator).toBe(true);
    expect(hook.result.current.newResponseIndicatorLabel).toBe(
      "Jump to latest",
    );

    removeListener();
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

  it("keeps history unpinned and reattaches through one bounded tail demand", async () => {
    const animationFrames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", ((
      callback: FrameRequestCallback,
    ) => {
      animationFrames.push(callback);
      return animationFrames.length;
    }) as typeof requestAnimationFrame);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const scrollNode = document.createElement("section");
    scrollNode.innerHTML = '<div class="virtualized-message-list"></div>';
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const demands: SessionHistoryPageDemand[] = [];
    const removeListener = addSessionHistoryPageDemandListener((demand) => {
      demands.push(demand);
    });
    const historicalSession = session(true);
    const sharedParams = {
      ...params(historicalSession),
      isSessionTabActive: true,
    };
    const hook = renderHook(
      ({ activeSession }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession,
        }),
      { initialProps: { activeSession: historicalSession } },
    );
    hook.result.current.messageStackRef.current = scrollNode;

    expect(hook.result.current.liveTailPinned).toBe(false);
    expect(hook.result.current.showNewResponseIndicator).toBe(true);
    expect(hook.result.current.newResponseIndicatorLabel).toBe(
      "Jump to latest",
    );

    await act(async () => {
      hook.result.current.scrollMessageStackToBoundary("bottom");
      hook.result.current.scrollMessageStackToBoundary("bottom");
      const demand = demands[0];
      expect(demand).toMatchObject({
        sessionId: "session-history",
        direction: "tail",
      });
      completeSessionHistoryPageDemand(demand?.requestId, true);
      await Promise.resolve();
    });
    expect(animationFrames).toHaveLength(1);
    act(() => {
      animationFrames.shift()?.(0);
    });
    hook.rerender({ activeSession: session(false) });

    expect(demands).toHaveLength(1);
    expect(hook.result.current.liveTailPinned).toBe(true);
    expect(hook.result.current.showNewResponseIndicator).toBe(false);
    expect(scrollNode.scrollTop).toBe(800);

    removeListener();
  });

  it("honors jump-to-start against the resident window when page adoption fails", async () => {
    const animationFrames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", ((
      callback: FrameRequestCallback,
    ) => {
      animationFrames.push(callback);
      return animationFrames.length;
    }) as typeof requestAnimationFrame);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    scrollNode.scrollTo = vi.fn(
      (optionsOrX?: ScrollToOptions | number, y?: number) => {
        scrollNode.scrollTop =
          typeof optionsOrX === "number"
            ? (y ?? scrollNode.scrollTop)
            : (optionsOrX?.top ?? scrollNode.scrollTop);
      },
    ) as typeof scrollNode.scrollTo;
    const demands: SessionHistoryPageDemand[] = [];
    const removeListener = addSessionHistoryPageDemandListener((demand) => {
      demands.push(demand);
    });
    const initialHistoricalWindow = session(true);
    const sharedParams = {
      ...params(initialHistoricalWindow),
      isSessionTabActive: true,
    };
    const hook = renderHook(
      ({ activeSession }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          activeSession,
        }),
      { initialProps: { activeSession: initialHistoricalWindow } },
    );
    hook.result.current.messageStackRef.current = scrollNode;
    hook.rerender({
      activeSession: {
        ...session(false),
        hasOlderHistory: undefined,
      },
    });

    act(() => {
      hook.result.current.scrollMessageStackToBoundary("top");
      hook.result.current.scrollMessageStackToBoundary("top");
    });
    const demand = demands[0];
    expect(demand).toMatchObject({
      sessionId: "session-history",
      direction: "start",
    });
    act(() => {
      completeSessionHistoryPageDemand(demand?.requestId, false);
    });
    await Promise.resolve();

    expect(demands).toHaveLength(1);
    expect(animationFrames).toHaveLength(1);
    act(() => {
      animationFrames.shift()?.(0);
    });
    expect(scrollNode.scrollTop).toBe(0);

    removeListener();
  });

  it("does not let a stale same-key boundary demand block or clear a newer request", async () => {
    vi.stubGlobal(
      "requestAnimationFrame",
      vi.fn(() => 1),
    );
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    scrollNode.scrollTo = vi.fn() as typeof scrollNode.scrollTo;
    const demands: SessionHistoryPageDemand[] = [];
    const removeListener = addSessionHistoryPageDemandListener((demand) => {
      demands.push(demand);
    });
    const activeSession = session(false);
    const sharedParams = {
      ...params(activeSession),
      isSessionTabActive: true,
    };
    const hook = renderHook(
      ({ scrollStateKey }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          scrollStateKey,
        }),
      { initialProps: { scrollStateKey: "pane-1:session-history" } },
    );
    hook.result.current.messageStackRef.current = scrollNode;

    act(() => {
      hook.result.current.scrollMessageStackToBoundary("top");
    });
    expect(demands).toHaveLength(1);

    hook.rerender({ scrollStateKey: "pane-1:other-session" });
    hook.rerender({ scrollStateKey: "pane-1:session-history" });
    act(() => {
      hook.result.current.scrollMessageStackToBoundary("top");
    });
    expect(demands).toHaveLength(2);

    act(() => {
      completeSessionHistoryPageDemand(demands[0]?.requestId, false);
    });
    await Promise.resolve();
    act(() => {
      hook.result.current.scrollMessageStackToBoundary("top");
    });
    expect(
      demands,
      "the old completion must not clear the newer in-flight demand",
    ).toHaveLength(2);

    act(() => {
      completeSessionHistoryPageDemand(demands[1]?.requestId, false);
    });
    await Promise.resolve();
    act(() => {
      hook.result.current.scrollMessageStackToBoundary("top");
    });
    expect(demands).toHaveLength(3);

    act(() => {
      completeSessionHistoryPageDemand(demands[2]?.requestId, false);
    });
    removeListener();
  });

  it("keeps the newer tail boundary when a start request completes last", async () => {
    const animationFrames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", ((
      callback: FrameRequestCallback,
    ) => {
      animationFrames.push(callback);
      return animationFrames.length;
    }) as typeof requestAnimationFrame);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const scrollNode = document.createElement("section");
    scrollNode.innerHTML = '<div class="virtualized-message-list"></div>';
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    scrollNode.scrollTo = vi.fn(
      (optionsOrX?: ScrollToOptions | number, y?: number) => {
        scrollNode.scrollTop =
          typeof optionsOrX === "number"
            ? (y ?? scrollNode.scrollTop)
            : (optionsOrX?.top ?? scrollNode.scrollTop);
      },
    ) as typeof scrollNode.scrollTo;
    const demands: SessionHistoryPageDemand[] = [];
    const removeListener = addSessionHistoryPageDemandListener((demand) => {
      demands.push(demand);
    });
    const windowedSession = {
      ...session(true),
      hasOlderHistory: true,
    };
    const hook = renderHook(() =>
      useSessionPaneScrollState({
        ...params(windowedSession),
        isSessionTabActive: true,
      }),
    );
    hook.result.current.messageStackRef.current = scrollNode;

    try {
      act(() => {
        hook.result.current.scrollMessageStackToBoundary("top");
        hook.result.current.scrollMessageStackToBoundary("bottom");
      });
      expect(demands.map((demand) => demand.direction)).toEqual([
        "start",
        "tail",
      ]);

      await act(async () => {
        completeSessionHistoryPageDemand(demands[1]?.requestId, true);
        await Promise.resolve();
      });
      expect(animationFrames).toHaveLength(1);

      await act(async () => {
        completeSessionHistoryPageDemand(demands[0]?.requestId, true);
        await Promise.resolve();
      });
      expect(
        animationFrames,
        "the stale start completion must not enqueue a second boundary write",
      ).toHaveLength(1);

      act(() => {
        animationFrames.shift()?.(0);
      });
      expect(scrollNode.scrollTop).toBe(800);
    } finally {
      hook.unmount();
      removeListener();
    }
  });

  it("does not apply a start boundary after intervening manual paging", async () => {
    const animationFrames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", ((
      callback: FrameRequestCallback,
    ) => {
      animationFrames.push(callback);
      return animationFrames.length;
    }) as typeof requestAnimationFrame);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const scrollNode = document.createElement("section");
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    scrollNode.scrollTo = vi.fn(
      (optionsOrX?: ScrollToOptions | number, y?: number) => {
        scrollNode.scrollTop =
          typeof optionsOrX === "number"
            ? (y ?? scrollNode.scrollTop)
            : (optionsOrX?.top ?? scrollNode.scrollTop);
      },
    ) as typeof scrollNode.scrollTo;
    const demands: SessionHistoryPageDemand[] = [];
    const removeListener = addSessionHistoryPageDemandListener((demand) => {
      demands.push(demand);
    });
    const historicalSession = session(false);
    const hook = renderHook(() =>
      useSessionPaneScrollState({
        ...params(historicalSession),
        isSessionTabActive: true,
      }),
    );
    hook.result.current.messageStackRef.current = scrollNode;

    try {
      act(() => {
        hook.result.current.scrollMessageStackToBoundary("top");
      });
      expect(demands).toHaveLength(1);

      act(() => {
        hook.result.current.scrollMessageStackByPage(-1);
      });
      expect(scrollNode.scrollTop).toBe(630);

      await act(async () => {
        completeSessionHistoryPageDemand(demands[0]?.requestId, false);
        await Promise.resolve();
      });
      expect(animationFrames).toHaveLength(0);
      expect(scrollNode.scrollTop).toBe(630);
    } finally {
      hook.unmount();
      removeListener();
    }
  });
});
