// Owns explicit history boundaries, demand races and sending-status navigation.
// Does not own keyboard/wheel policy or App integration.
// Split from SessionPaneView.scroll.test.ts.
import { act, renderHook } from "@testing-library/react";
import type { UIEvent as ReactUIEvent } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useSessionPaneScrollState } from "./SessionPaneView.scroll";
import {
  MESSAGE_STACK_SCROLL_WRITE_EVENT,
  MESSAGE_STACK_USER_SCROLL_INTENT_EVENT,
  claimMessageStackNativeScrollOwnership,
} from "./message-stack-scroll-sync";
import {
  addSessionHistoryPageDemandListener,
  completeSessionHistoryPageDemand,
  type SessionHistoryPageDemand,
} from "./session-history-demand";
import type { Message } from "./types";

import {
  installAnimationFrameHarness,
  params,
  session,
} from "./SessionPaneView.scroll.fixtures";

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("session pane scroll boundaries", () => {
  it.each(["layout", "pointer", "manual", "finished"] as const)(
    "preserves boundary FOLLOW across a transient height clamp, not newer input (%s)",
    (cause) => {
      installAnimationFrameHarness(1000 / 60);
      const shared = params(session(false));
      const key = shared.scrollStateKey;
      const paneShouldStickToBottomRef = { current: { [key]: false } };
      const node = document.createElement("section");
      node.innerHTML = '<div class="virtualized-message-list"></div>';
      let height = 1000;
      Object.defineProperties(node, {
        clientHeight: { value: 200 },
        scrollHeight: { get: () => height },
        scrollTop: { configurable: true, writable: true, value: 400 },
      });
      node.scrollTo = vi.fn((options: ScrollToOptions | number) => {
        if (typeof options !== "number") {
          node.scrollTop = options.top ?? 0;
        }
      }) as typeof node.scrollTo;
      // The real virtualizer publishes this while its bounded reveal owns
      // mounting/measurement, and removes it on completion or newer input.
      node.addEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, (event) => {
        if ((event as CustomEvent).detail?.scrollKind === "bottom_boundary") {
          node.dataset.virtualizedBottomBoundaryReveal = "true";
        }
      });
      const userIntent = vi.fn();
      node.addEventListener(MESSAGE_STACK_USER_SCROLL_INTENT_EVENT, userIntent);
      const hook = renderHook(
        ({ visible }) => useSessionPaneScrollState({
          ...shared,
          isActive: true,
          isSessionTabActive: visible,
          paneShouldStickToBottomRef,
        }),
        { initialProps: { visible: false } },
      );
      hook.result.current.messageStackRef.current = node;
      hook.rerender({ visible: true });
      act(() => hook.result.current.scrollMessageStackToBoundary("bottom"));
      expect(node.scrollTop).toBe(800);
      expect(paneShouldStickToBottomRef.current[key]).toBe(true);

      act(() => {
        if (cause === "pointer") {
          claimMessageStackNativeScrollOwnership(
            node,
            { owner: "pointer", direction: null },
            5000,
          );
        } else if (cause === "manual") {
          hook.result.current.scrollMessageStackByPage(-1);
        } else if (cause === "finished") {
          delete node.dataset.virtualizedBottomBoundaryReveal;
        }
        // Chromium clamps synchronously while the mounted range shrinks;
        // its native scroll event can arrive after the height grows back.
        height = 600;
        node.scrollTop = height - node.clientHeight;
        height = 1000;
        hook.result.current.handleMessageStackScroll({
          currentTarget: node,
          nativeEvent: new Event("scroll"),
        } as ReactUIEvent<HTMLElement>);
      });

      expect(paneShouldStickToBottomRef.current[key]).toBe(cause === "layout");
      if (cause === "layout") {
        expect(userIntent).not.toHaveBeenCalled();
        expect(hook.result.current.liveTailPinned).toBe(true);
      }
      hook.unmount();
    },
  );

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

  it("does not infer prompt navigation from sending status in a historical window", () => {
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
