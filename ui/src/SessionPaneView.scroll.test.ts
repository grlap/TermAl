import { act, renderHook } from "@testing-library/react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  canMoveMessageStackByDelta,
  claimMessageStackBottomRepinAuthority,
  isFirstAgentOutputForObservedPrompt,
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
  MESSAGE_STACK_SCROLL_WRITE_EVENT,
  requestMessageStackBottomRepin,
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
    forceSessionScrollToBottomRef: { current: {} },
    hasSessionFindQuery: false,
    isActive: false,
    isSending: false,
    isSessionTabActive: false,
    onScrollToBottomRequestHandled: vi.fn(),
    paneContentSignatures: {},
    paneId: "pane-1",
    paneMessageContentSignatures: {},
    paneRootRef: { current: null },
    paneScrollPositions: {},
    paneShouldStickToBottomRef: { current: {} },
    paneViewMode: "session" as const,
    pendingScrollToBottomRequest: null,
    scrollStateKey: "pane-1:session-history",
    sessions: [activeSession],
    showWaitingIndicator: false,
    visibleContentSignature: "history",
    visibleLastMessageAuthor: "assistant" as const,
    visibleMessageContentSignature: "history-message",
  };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("session pane historical-window tail state", () => {
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
    const paneShouldStickToBottomRef = { current: { "pane-1": false } };
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

  it("coalesces live commits, survives status changes, and guarantees convergence", () => {
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
    const paneShouldStickToBottomRef = { current: { "pane-1": true } };
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
    let setupFrameTimestamp = 0;
    let setupFrameCount = 0;
    while (animationFrames.size > 0 && setupFrameCount < 10) {
      const setupFrame = animationFrames.entries().next().value;
      if (!setupFrame) {
        break;
      }
      animationFrames.delete(setupFrame[0]);
      setupFrameTimestamp += 1000 / 60;
      act(() => setupFrame[1](setupFrameTimestamp));
      setupFrameCount += 1;
    }
    expect(setupFrameCount).toBeLessThan(10);
    animationFrames.clear();
    requestAnimationFrame.mockClear();
    scrollTo.mockClear();

    scrollHeight = 1_120;
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

    expect(scrollNode.scrollTop).toBe(800);
    expect(scrollTo).not.toHaveBeenCalled();
    expect(requestAnimationFrame).toHaveBeenCalledTimes(1);
    expect(animationFrames.size).toBe(1);

    // A second committed delta before the browser drains rAF only retargets
    // the existing controller. Commit frequency cannot consume extra motion
    // budget or enqueue another animation frame.
    scrollHeight = 1_240;
    scrollTo.mockClear();
    const streamingSession = {
      ...activeSession,
      messages: [
        prompt,
        {
          ...firstReply,
          text: "First reply with another streamed line",
        },
      ],
      messageCount: 2,
    };
    hook.rerender({
      currentSession: streamingSession,
      contentSignature: "reply-current:stream-2",
      paneActive: true,
      waiting: true,
    });

    expect(scrollNode.scrollTop).toBe(800);
    expect(scrollTo).not.toHaveBeenCalled();
    expect(requestAnimationFrame).toHaveBeenCalledTimes(1);
    expect(animationFrames.size).toBe(1);

    // Measurement requests join the active follow loop instead of adding a
    // second synchronous writer for the same growth.
    scrollTo.mockClear();
    act(() => {
      requestMessageStackBottomRepin(scrollNode);
    });
    expect(scrollTo).not.toHaveBeenCalled();
    const firstFrame = animationFrames.entries().next().value;
    if (!firstFrame) {
      throw new Error("Expected a scheduled bottom-follow frame");
    }
    animationFrames.delete(firstFrame[0]);
    let frameTimestamp = 1_000;
    act(() => firstFrame[1](frameTimestamp));

    const firstFrameScrollTop = scrollNode.scrollTop;
    expect(firstFrameScrollTop).toBeGreaterThan(800);
    expect(firstFrameScrollTop - 800).toBeLessThan(50);
    expect(scrollTo).toHaveBeenCalledWith({
      behavior: "auto",
      top: firstFrameScrollTop,
    });
    expect(scrollWrites[scrollWrites.length - 1]?.detail.scrollKind).toBe(
      "bottom_follow",
    );

    // A status-only commit runs the content effect again but must not cancel
    // the in-flight controller when there is no replacement schedule.
    const pendingFrameIdAfterFirstWrite = animationFrames.keys().next().value;
    if (pendingFrameIdAfterFirstWrite === undefined) {
      throw new Error("Expected bottom-follow to schedule its next frame");
    }
    hook.rerender({
      currentSession: streamingSession,
      contentSignature: "reply-current:stream-2",
      paneActive: true,
      waiting: false,
    });
    expect(animationFrames.size).toBe(1);
    expect(animationFrames.has(pendingFrameIdAfterFirstWrite)).toBe(true);

    // A plain live commit can shrink the physical bottom and let the browser
    // clamp scrollTop before the layout effect runs. Even without an explicit
    // beforePaint repin request, the virtualizer must receive a synchronous
    // bottom-follow notification while no upward write is issued.
    scrollHeight = 980;
    scrollNode.scrollTop = 780;
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

    // A very large final card exceeds the normal 60-frame velocity budget.
    // The controller remains smooth during that budget and then performs its
    // explicit terminal correction so attached content cannot stay hidden.
    scrollHeight = 5_000;
    hook.rerender({
      currentSession: {
        ...streamingSession,
        messages: [
          prompt,
          {
            ...firstReply,
            text: "A very large final response",
          },
        ],
      },
      contentSignature: "reply-current:very-large-final",
      paneActive: true,
      waiting: false,
    });
    let drainedFrames = 0;
    while (animationFrames.size > 0 && drainedFrames < 70) {
      const nextFrame = animationFrames.entries().next().value;
      if (!nextFrame) {
        break;
      }
      animationFrames.delete(nextFrame[0]);
      frameTimestamp += 1000 / 60;
      act(() => nextFrame[1](frameTimestamp));
      drainedFrames += 1;
    }
    expect(drainedFrames).toBeLessThanOrEqual(60);
    expect(scrollNode.scrollTop).toBe(4_800);

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

    paneShouldStickToBottomRef.current["pane-1"] = false;
    scrollNode.scrollTop = 700;
    scrollHeight = 1_240;
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

  it("does not detach live follow for navigation keys owned by transcript controls", () => {
    const activeSession = session(false);
    const scrollNode = document.createElement("section");
    const button = document.createElement("button");
    scrollNode.append(button);
    const sharedParams = {
      ...params(activeSession),
      paneShouldStickToBottomRef: { current: { "pane-1": true } },
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
        target: scrollNode,
        type: "keydown",
      } as unknown as ReactKeyboardEvent<HTMLElement>);
    });
    expect(hook.result.current.liveTailPinned).toBe(true);
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
        "pane-1": true,
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
      ({ isSessionTabActive }) =>
        useSessionPaneScrollState({
          ...sharedParams,
          isSessionTabActive,
        }),
      { initialProps: { isSessionTabActive: false } },
    );
    hook.result.current.messageStackRef.current = scrollNode;

    hook.rerender({ isSessionTabActive: true });

    expect(scrollNode.scrollTop).toBe(52_788);
    expect(paneShouldStickToBottomRef.current["pane-1"]).toBe(true);
    expect(animationFrames.length).toBeGreaterThan(0);
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
      paneShouldStickToBottomRef: { current: { "pane-1": true } },
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
      paneShouldStickToBottomRef: { current: { "pane-1": true } },
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
    expect(animationFrames).toHaveLength(1);

    act(() => {
      animationFrames.shift()?.(0);
    });

    expect(demands).toHaveLength(0);
    expect(hook.result.current.liveTailPinned).toBe(false);
    expect(hook.result.current.showNewResponseIndicator).toBe(true);
    expect(hook.result.current.newResponseIndicatorLabel).toBe(
      "Jump to latest",
    );

    removeListener();
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
    const paneShouldStickToBottomRef = { current: { "pane-1": false } };
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
    expect(paneShouldStickToBottomRef.current["pane-1"]).toBe(false);
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
    const sharedParams = params(historicalSession);
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
    const sharedParams = params(initialHistoricalWindow);
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
    const sharedParams = params(activeSession);
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
});
