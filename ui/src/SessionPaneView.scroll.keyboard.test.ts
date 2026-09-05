// Owns: body-keyboard ownership and selection-ownership coverage for the
// session pane message stack: which element owns Home/End/Arrow/Page keys,
// selection-extension keys staying browser-owned, boundary history demands,
// dialog gating, nested scrollers, and the macOS Command+Arrow contract.
// Does not own: tail-follow, bottom-follow, wheel, touch, or search-match
// scroll behaviour, which stay in SessionPaneView.scroll.test.ts.
// Split from: ui/src/SessionPaneView.scroll.test.ts.

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
  isMessageStackAtPhysicalBottom,
  resolveNewResponseIndicatorVisibility,
  resolveSessionPageScrollDistance,
  useSessionPaneScrollState,
} from "./SessionPaneView.scroll";
import {
  isFirstAgentOutputForObservedPrompt,
  resolveLatestTurnOutputState,
  resolveLatestTurnTailSignature,
  resolvePostLiveMessageFollowTransition,
  resolveSessionBottomFollowPersistedScrollTop,
  resolveSessionBottomFollowScrollTop,
  resolveSessionBottomFollowWriteScrollTop,
} from "./session-live-tail-follow";
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

describe("session pane body-keyboard and selection ownership", () => {
  it("does not treat an active embedded editor as body-keyboard ownership", async () => {
    const scrollNode = document.createElement("section");
    const input = document.createElement("textarea");
    scrollNode.append(input);
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
    const hook = renderHook(() =>
      useSessionPaneScrollState({
        ...params(session(false)),
        isActive: true,
        isSessionTabActive: true,
      }),
    );
    hook.result.current.messageStackRef.current = scrollNode;

    try {
      await act(async () => {
        fireEvent.mouseDown(input);
        input.focus();
        await Promise.resolve();
      });
      act(() => {
        fireEvent.keyDown(document.body, { key: "ArrowUp" });
      });
      expect(document.activeElement).toBe(input);
      expect(intentListener).not.toHaveBeenCalled();
    } finally {
      hook.unmount();
      scrollNode.remove();
    }
  });

  it("owns body-targeted ArrowUp over a residual downward wheel burst", () => {
    let now = 1_000;
    vi.spyOn(performance, "now").mockImplementation(() => now);
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
    const writeListener = vi.fn();
    scrollNode.addEventListener(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      intentListener,
    );
    scrollNode.addEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, writeListener);
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

    try {
      const keyEvent = withInputTimestamp(
        new KeyboardEvent("keydown", {
          bubbles: true,
          cancelable: true,
          key: "ArrowUp",
        }),
        now + 2,
      );
      const boundaryWheel = withInputTimestamp(
        new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          deltaY: 40,
        }),
        now,
      );
      act(() => {
        fireEvent.mouseDown(messageCard);
        // A downward wheel at the physical bottom cannot move, but its burst
        // can still have inertial ticks pending behind the newer key.
        scrollNode.dispatchEvent(boundaryWheel);
        now += 2;
        document.body.dispatchEvent(keyEvent);
      });

      expect(keyEvent.defaultPrevented).toBe(true);
      expect(scrollNode.scrollTop).toBe(760);
      expect(intentListener).toHaveBeenCalledTimes(1);
      expect(writeListener).toHaveBeenCalledTimes(1);
      expect((writeListener.mock.calls[0]?.[0] as CustomEvent).detail).toEqual({
        scrollKind: "incremental",
        scrollSource: "user",
      });
      expect(hook.result.current.liveTailPinned).toBe(false);

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
      expect(scrollNode.scrollTop).toBe(760);
      expect(hook.result.current.liveTailPinned).toBe(false);

      now += 10;
      const decayingResidualWheel = withInputTimestamp(
        new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          deltaY: 24,
        }),
        now,
      );
      act(() => {
        scrollNode.dispatchEvent(decayingResidualWheel);
      });
      expect(decayingResidualWheel.defaultPrevented).toBe(true);
      expect(scrollNode.scrollTop).toBe(760);

      now += 10;
      const firstAcceleratingWheel = withInputTimestamp(
        new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          deltaY: 32,
        }),
        now,
      );
      act(() => {
        scrollNode.dispatchEvent(firstAcceleratingWheel);
      });
      expect(firstAcceleratingWheel.defaultPrevented).toBe(true);
      expect(scrollNode.scrollTop).toBe(760);

      now += 10;
      const secondAcceleratingWheel = withInputTimestamp(
        new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          deltaY: 40,
        }),
        now,
      );
      act(() => {
        scrollNode.dispatchEvent(secondAcceleratingWheel);
      });
      expect(secondAcceleratingWheel.defaultPrevented).toBe(true);
      expect(scrollNode.scrollTop).toBe(800);

      const nestedScroller = document.createElement("div");
      nestedScroller.style.overflowY = "auto";
      Object.defineProperties(nestedScroller, {
        clientHeight: { configurable: true, value: 100 },
        scrollHeight: { configurable: true, value: 200 },
        scrollTop: { configurable: true, writable: true, value: 0 },
      });
      messageCard.append(nestedScroller);
      const nestedWheel = withInputTimestamp(
        new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          deltaY: 40,
        }),
        now,
      );
      act(() => {
        nestedScroller.dispatchEvent(nestedWheel);
      });
      expect(nestedWheel.defaultPrevented).toBe(false);
      expect(scrollNode.scrollTop).toBe(800);
    } finally {
      hook.unmount();
      scrollNode.remove();
    }
  });

  it("keeps body-keyboard ownership exclusive when an inactive pane is activated", () => {
    const liveSession = session(false);
    const firstNode = document.createElement("section");
    const firstMessage = document.createElement("article");
    firstNode.append(firstMessage);
    const secondNode = document.createElement("section");
    const secondMessage = document.createElement("article");
    secondNode.append(secondMessage);
    document.body.append(firstNode, secondNode);
    for (const node of [firstNode, secondNode]) {
      Object.defineProperties(node, {
        clientHeight: { configurable: true, value: 200 },
        scrollHeight: { configurable: true, value: 1_000 },
        scrollTop: { configurable: true, writable: true, value: 800 },
      });
    }
    const firstIntentListener = vi.fn();
    const secondIntentListener = vi.fn();
    firstNode.addEventListener(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      firstIntentListener,
    );
    secondNode.addEventListener(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      secondIntentListener,
    );
    const firstHook = renderHook(
      ({ isActive }) =>
        useSessionPaneScrollState({
          ...params(liveSession),
          isActive,
          isSessionTabActive: true,
          scrollStateKey: "pane-1:session-history",
        }),
      { initialProps: { isActive: true } },
    );
    const secondHook = renderHook(
      ({ isActive }) =>
        useSessionPaneScrollState({
          ...params(liveSession),
          isActive,
          isSessionTabActive: true,
          scrollStateKey: "pane-2:session-history",
        }),
      { initialProps: { isActive: false } },
    );
    firstHook.result.current.messageStackRef.current = firstNode;
    secondHook.result.current.messageStackRef.current = secondNode;

    try {
      act(() => {
        fireEvent.mouseDown(firstMessage);
        fireEvent.mouseDown(secondMessage);
      });
      firstHook.rerender({ isActive: false });
      secondHook.rerender({ isActive: true });
      act(() => {
        fireEvent.keyDown(document.body, { key: "ArrowUp" });
      });

      expect(firstIntentListener).not.toHaveBeenCalled();
      expect(secondIntentListener).toHaveBeenCalledTimes(1);
    } finally {
      firstHook.unmount();
      secondHook.unmount();
      firstNode.remove();
      secondNode.remove();
    }
  });

  it("restores pointer ownership after a focused transcript control unmounts", async () => {
    const liveSession = session(false);
    const scrollNode = document.createElement("section");
    const button = document.createElement("button");
    scrollNode.append(button);
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
    const hook = renderHook(() =>
      useSessionPaneScrollState({
        ...params(liveSession),
        isActive: true,
        isSessionTabActive: true,
      }),
    );
    hook.result.current.messageStackRef.current = scrollNode;

    try {
      act(() => {
        // Safari/Firefox on macOS may leave focus on body after this click.
        fireEvent.mouseDown(button);
        fireEvent.keyDown(document.body, { key: "ArrowUp" });
      });
      expect(intentListener).toHaveBeenCalledTimes(1);

      await act(async () => {
        button.focus();
        await Promise.resolve();
      });
      act(() => {
        fireEvent.keyDown(document.body, { key: "ArrowUp" });
      });
      expect(intentListener).toHaveBeenCalledTimes(1);

      const activeElementSpy = vi
        .spyOn(document, "activeElement", "get")
        .mockReturnValue(null);
      try {
        act(() => {
          button.remove();
          fireEvent.keyDown(document.body, { key: "ArrowUp" });
        });
      } finally {
        activeElementSpy.mockRestore();
      }
      expect(intentListener).toHaveBeenCalledTimes(2);
    } finally {
      hook.unmount();
      scrollNode.remove();
    }
  });

  it("does not restore transcript ownership after focus deliberately moves outside", async () => {
    const scrollNode = document.createElement("section");
    const messageCard = document.createElement("article");
    const composer = document.createElement("textarea");
    scrollNode.append(messageCard);
    document.body.append(scrollNode, composer);
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
    const hook = renderHook(() =>
      useSessionPaneScrollState({
        ...params(session(false)),
        isActive: true,
        isSessionTabActive: true,
      }),
    );
    hook.result.current.messageStackRef.current = scrollNode;

    try {
      act(() => {
        fireEvent.mouseDown(messageCard);
      });
      await act(async () => {
        composer.focus();
        await Promise.resolve();
      });
      act(() => {
        composer.remove();
        fireEvent.keyDown(document.body, { key: "ArrowUp" });
      });
      expect(intentListener).not.toHaveBeenCalled();
    } finally {
      hook.unmount();
      composer.remove();
      scrollNode.remove();
    }
  });

  it("routes body-owned Home and End through one bounded history demand", () => {
    const exerciseBoundary = (
      activeSession: Session,
      key: "Home" | "End",
      expectedDirection: "start" | "tail" | null,
    ) => {
      const scrollNode = document.createElement("section");
      const messageCard = document.createElement("article");
      scrollNode.append(messageCard);
      document.body.append(scrollNode);
      Object.defineProperties(scrollNode, {
        clientHeight: { configurable: true, value: 200 },
        scrollHeight: { configurable: true, value: 1_000 },
        scrollTop: {
          configurable: true,
          writable: true,
          value: key === "Home" ? 800 : 0,
        },
      });
      scrollNode.scrollTo = vi.fn(
        (optionsOrX?: ScrollToOptions | number, y?: number) => {
          scrollNode.scrollTop =
            typeof optionsOrX === "number"
              ? (y ?? scrollNode.scrollTop)
              : (optionsOrX?.top ?? scrollNode.scrollTop);
        },
      ) as typeof scrollNode.scrollTo;
      const normalizedIntentListener = vi.fn();
      scrollNode.addEventListener(
        MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
        normalizedIntentListener,
      );
      const demands: SessionHistoryPageDemand[] = [];
      const removeDemandListener = addSessionHistoryPageDemandListener(
        (demand) => demands.push(demand),
      );
      const hook = renderHook(() =>
        useSessionPaneScrollState({
          ...params(activeSession),
          isActive: true,
          isSessionTabActive: true,
        }),
      );
      hook.result.current.messageStackRef.current = scrollNode;

      try {
        act(() => {
          fireEvent.mouseDown(messageCard);
          fireEvent.keyDown(document.body, { key });
        });

        expect(normalizedIntentListener).not.toHaveBeenCalled();
        if (expectedDirection === null) {
          expect(demands).toEqual([]);
        } else {
          expect(demands).toHaveLength(1);
          expect(demands[0]).toMatchObject({
            direction: expectedDirection,
            sessionId: "session-history",
          });
        }
      } finally {
        const cleanupRequestId = demands[0]?.requestId;
        if (cleanupRequestId !== undefined) {
          act(() => {
            completeSessionHistoryPageDemand(cleanupRequestId, false);
          });
        }
        hook.unmount();
        removeDemandListener();
        scrollNode.remove();
      }
    };

    exerciseBoundary(session(false), "Home", "start");
    exerciseBoundary(session(true), "End", "tail");
    exerciseBoundary({ ...session(true), hasOlderHistory: undefined }, "Home", null);
    exerciseBoundary({ ...session(true), hasOlderHistory: false }, "Home", null);
    exerciseBoundary(
      { ...session(true), hasOlderHistory: true },
      "Home",
      "start",
    );
    exerciseBoundary(
      {
        ...session(true),
        hasOlderHistory: true,
        messageCount: 2,
      },
      "Home",
      "start",
    );
  });

  it("honors preventDefault before body-owned boundary navigation", () => {
    const scrollNode = document.createElement("section");
    const messageCard = document.createElement("article");
    scrollNode.append(messageCard);
    document.body.append(scrollNode);
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    const demands: SessionHistoryPageDemand[] = [];
    const removeDemandListener = addSessionHistoryPageDemandListener(
      (demand) => demands.push(demand),
    );
    const preventBoundary = (event: KeyboardEvent) => {
      if (event.key === "Home") {
        event.preventDefault();
      }
    };
    document.addEventListener("keydown", preventBoundary);
    const hook = renderHook(() =>
      useSessionPaneScrollState({
        ...params(session(false)),
        isActive: true,
        isSessionTabActive: true,
      }),
    );
    hook.result.current.messageStackRef.current = scrollNode;

    try {
      act(() => {
        fireEvent.mouseDown(messageCard);
        fireEvent.keyDown(document.body, { key: "Home" });
      });
      expect(demands).toEqual([]);
      expect(scrollNode.scrollTop).toBe(800);
    } finally {
      hook.unmount();
      document.removeEventListener("keydown", preventBoundary);
      removeDemandListener();
      scrollNode.remove();
    }
  });

  it("keeps body-owned selection-extension keys out of scroll intent", () => {
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
    const demands: SessionHistoryPageDemand[] = [];
    const removeDemandListener = addSessionHistoryPageDemandListener(
      (demand) => demands.push(demand),
    );
    const hook = renderHook(() =>
      useSessionPaneScrollState({
        ...params(session(false)),
        isActive: true,
        isSessionTabActive: true,
      }),
    );
    hook.result.current.messageStackRef.current = scrollNode;

    try {
      let shiftHomeContinues = false;
      let shiftArrowUpContinues = false;
      let shiftPageUpContinues = false;
      act(() => {
        fireEvent.mouseDown(messageCard);
        shiftHomeContinues = fireEvent.keyDown(document.body, {
          key: "Home",
          shiftKey: true,
        });
        shiftArrowUpContinues = fireEvent.keyDown(document.body, {
          key: "ArrowUp",
          shiftKey: true,
        });
        shiftPageUpContinues = fireEvent.keyDown(document.body, {
          key: "PageUp",
          shiftKey: true,
        });
      });
      expect(shiftHomeContinues).toBe(true);
      expect(shiftArrowUpContinues).toBe(true);
      expect(shiftPageUpContinues).toBe(true);
      expect(intentListener).not.toHaveBeenCalled();
      expect(demands).toEqual([]);
      expect(scrollNode.scrollTop).toBe(800);
      expect(hook.result.current.liveTailPinned).toBe(true);
    } finally {
      hook.unmount();
      removeDemandListener();
      scrollNode.remove();
    }
  });

  it("routes body-owned Ctrl+Shift+ArrowUp through the bounded start demand and keeps Ctrl+Shift+Home browser-owned", () => {
    // Ctrl+Shift+Arrow is the pane's boundary shortcut and must behave
    // exactly like plain Home here; Ctrl+Shift+Home stays a browser
    // select-to-document-start gesture and must not raise any demand.
    const scrollNode = document.createElement("section");
    const messageCard = document.createElement("article");
    scrollNode.append(messageCard);
    document.body.append(scrollNode);
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
    const intentListener = vi.fn();
    scrollNode.addEventListener(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      intentListener,
    );
    const demands: SessionHistoryPageDemand[] = [];
    const removeDemandListener = addSessionHistoryPageDemandListener(
      (demand) => demands.push(demand),
    );
    const hook = renderHook(() =>
      useSessionPaneScrollState({
        ...params(session(false)),
        isActive: true,
        isSessionTabActive: true,
      }),
    );
    hook.result.current.messageStackRef.current = scrollNode;

    try {
      let ctrlShiftHomeContinues = false;
      act(() => {
        fireEvent.mouseDown(messageCard);
        ctrlShiftHomeContinues = fireEvent.keyDown(document.body, {
          key: "Home",
          ctrlKey: true,
          shiftKey: true,
        });
      });
      expect(ctrlShiftHomeContinues).toBe(true);
      expect(demands).toEqual([]);
      expect(hook.result.current.liveTailPinned).toBe(true);

      let ctrlShiftArrowUpContinues = true;
      act(() => {
        ctrlShiftArrowUpContinues = fireEvent.keyDown(document.body, {
          key: "ArrowUp",
          ctrlKey: true,
          shiftKey: true,
        });
      });
      expect(ctrlShiftArrowUpContinues).toBe(false);
      expect(demands).toHaveLength(1);
      expect(demands[0]).toMatchObject({
        direction: "start",
        sessionId: "session-history",
      });
      expect(hook.result.current.liveTailPinned).toBe(false);
    } finally {
      const requestId = demands[0]?.requestId;
      if (requestId !== undefined) {
        act(() => {
          completeSessionHistoryPageDemand(requestId, false);
        });
      }
      hook.unmount();
      removeDemandListener();
      scrollNode.remove();
    }
  });

  it("blocks body-owned scroll keys only for visible open dialogs", () => {
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
    const hook = renderHook(() =>
      useSessionPaneScrollState({
        ...params(session(false)),
        isActive: true,
        isSessionTabActive: true,
      }),
    );
    hook.result.current.messageStackRef.current = scrollNode;
    const hiddenDialogParent = document.createElement("div");
    hiddenDialogParent.style.display = "none";
    const hiddenDialog = document.createElement("section");
    hiddenDialog.setAttribute("aria-modal", "true");
    hiddenDialogParent.append(hiddenDialog);
    const nonModalPopover = document.createElement("section");
    nonModalPopover.setAttribute("role", "dialog");
    const dialog = document.createElement("section");
    dialog.setAttribute("aria-modal", "true");

    try {
      act(() => {
        fireEvent.mouseDown(messageCard);
        // The hidden dialog comes first in DOM order. It must neither block
        // transcript keys itself nor mask the visible modal after it.
        document.body.append(hiddenDialogParent);
        document.body.append(nonModalPopover);
        document.body.append(dialog);
        fireEvent.keyDown(document.body, { key: "ArrowUp" });
      });

      expect(intentListener).not.toHaveBeenCalled();
      expect(hook.result.current.liveTailPinned).toBe(true);
      expect(scrollNode.scrollTop).toBe(800);

      act(() => {
        dialog.remove();
        fireEvent.keyDown(document.body, { key: "ArrowUp" });
      });
      expect(intentListener).toHaveBeenCalledTimes(1);
      expect(hook.result.current.liveTailPinned).toBe(false);
    } finally {
      hook.unmount();
      dialog.remove();
      nonModalPopover.remove();
      hiddenDialogParent.remove();
      scrollNode.remove();
    }
  });

  it("keeps body-owned keys with the last-clicked nested scroller while it can move", () => {
    const scrollNode = document.createElement("section");
    const nestedScroller = document.createElement("div");
    const nestedContent = document.createElement("span");
    const focusedMessage = document.createElement("article");
    focusedMessage.tabIndex = -1;
    nestedScroller.style.overflowY = "auto";
    nestedScroller.append(nestedContent);
    scrollNode.append(nestedScroller, focusedMessage);
    document.body.append(scrollNode);
    Object.defineProperties(scrollNode, {
      clientHeight: { configurable: true, value: 200 },
      scrollHeight: { configurable: true, value: 1_000 },
      scrollTop: { configurable: true, writable: true, value: 800 },
    });
    Object.defineProperties(nestedScroller, {
      clientHeight: { configurable: true, value: 100 },
      scrollHeight: { configurable: true, value: 400 },
      scrollTop: { configurable: true, writable: true, value: 120 },
    });
    const intentListener = vi.fn();
    scrollNode.addEventListener(
      MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
      intentListener,
    );
    const hook = renderHook(() =>
      useSessionPaneScrollState({
        ...params(session(false)),
        isActive: true,
        isSessionTabActive: true,
      }),
    );
    hook.result.current.messageStackRef.current = scrollNode;

    try {
      act(() => {
        fireEvent.mouseDown(nestedContent);
        fireEvent.keyDown(document.body, { key: "ArrowUp" });
      });
      expect(intentListener).not.toHaveBeenCalled();
      expect(hook.result.current.liveTailPinned).toBe(true);

      act(() => {
        focusedMessage.focus();
        // A focus-based ownership grant supersedes the earlier pointer target.
        // The still-scrollable nested block must no longer suppress this key.
        fireEvent.keyDown(document.body, { key: "ArrowUp" });
      });
      expect(intentListener).toHaveBeenCalledTimes(1);
      expect(hook.result.current.liveTailPinned).toBe(false);

      act(() => {
        fireEvent.mouseDown(nestedContent);
        nestedScroller.scrollTop = 0;
        fireEvent.keyDown(document.body, { key: "ArrowUp" });
      });
      expect(intentListener).toHaveBeenCalledTimes(2);
    } finally {
      hook.unmount();
      scrollNode.remove();
    }
  });

  it("routes macOS body-owned Command+Arrow through bounded history demand", () => {
    const originalPlatform = Object.getOwnPropertyDescriptor(
      window.navigator,
      "platform",
    );
    const navigatorWithUserAgentData = window.navigator as Navigator & {
      userAgentData?: { platform?: string };
    };
    const originalUserAgentData = Object.getOwnPropertyDescriptor(
      navigatorWithUserAgentData,
      "userAgentData",
    );
    Object.defineProperty(window.navigator, "platform", {
      configurable: true,
      value: "MacIntel",
    });
    Object.defineProperty(navigatorWithUserAgentData, "userAgentData", {
      configurable: true,
      value: { platform: "macOS" },
    });
    try {
      const exerciseBoundary = (
        activeSession: Session,
        key: "ArrowUp" | "ArrowDown",
        expectedDirection: "start" | "tail",
      ) => {
        const scrollNode = document.createElement("section");
        const messageCard = document.createElement("article");
        scrollNode.append(messageCard);
        document.body.append(scrollNode);
        Object.defineProperties(scrollNode, {
          clientHeight: { configurable: true, value: 200 },
          scrollHeight: { configurable: true, value: 1_000 },
          scrollTop: {
            configurable: true,
            writable: true,
            value: key === "ArrowUp" ? 800 : 0,
          },
        });
        scrollNode.scrollTo = vi.fn() as typeof scrollNode.scrollTo;
        const normalizedIntentListener = vi.fn();
        scrollNode.addEventListener(
          MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
          normalizedIntentListener,
        );
        const demands: SessionHistoryPageDemand[] = [];
        const removeDemandListener = addSessionHistoryPageDemandListener(
          (demand) => demands.push(demand),
        );
        const hook = renderHook(() =>
          useSessionPaneScrollState({
            ...params(activeSession),
            isActive: true,
            isSessionTabActive: true,
          }),
        );
        hook.result.current.messageStackRef.current = scrollNode;

        try {
          let browserDefaultContinues = true;
          act(() => {
            fireEvent.mouseDown(messageCard);
            browserDefaultContinues = fireEvent.keyDown(document.body, {
              key,
              metaKey: true,
            });
          });

          expect(browserDefaultContinues).toBe(false);
          expect(normalizedIntentListener).not.toHaveBeenCalled();
          expect(demands).toHaveLength(1);
          expect(demands[0]).toMatchObject({
            direction: expectedDirection,
            sessionId: "session-history",
          });
        } finally {
          const cleanupRequestId = demands[0]?.requestId;
          if (cleanupRequestId !== undefined) {
            act(() => {
              completeSessionHistoryPageDemand(cleanupRequestId, false);
            });
          }
          hook.unmount();
          removeDemandListener();
          scrollNode.remove();
        }
      };

      exerciseBoundary(session(false), "ArrowUp", "start");
      exerciseBoundary(session(true), "ArrowDown", "tail");
    } finally {
      if (originalPlatform) {
        Object.defineProperty(window.navigator, "platform", originalPlatform);
      } else {
        Reflect.deleteProperty(window.navigator, "platform");
      }
      if (originalUserAgentData) {
        Object.defineProperty(
          navigatorWithUserAgentData,
          "userAgentData",
          originalUserAgentData,
        );
      } else {
        Reflect.deleteProperty(navigatorWithUserAgentData, "userAgentData");
      }
    }
  });
});
