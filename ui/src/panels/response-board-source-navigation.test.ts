import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { requestResponseBoardSourceNavigation } from "../response-board-navigation";
import { useResponseBoardSourceNavigation } from "./response-board-source-navigation";
import type {
  VirtualizedConversationMessageListHandle,
  VirtualizedConversationMessageListHandleRef,
} from "./virtualized-conversation-types";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

function makeVirtualizerHandle() {
  let userScrollGeneration = 0;
  const handle: VirtualizedConversationMessageListHandle = {
    beginUserScrollNavigation: vi.fn(() => {
      userScrollGeneration += 1;
      return userScrollGeneration;
    }),
    getLayoutSnapshot: vi.fn(),
    getUserScrollGeneration: vi.fn(() => userScrollGeneration),
    getViewportSnapshot: vi.fn(),
    jumpToMessageId: vi.fn(),
    jumpToMessageIndex: vi.fn(),
    restoreViewportAnchor: vi.fn(),
  };
  return {
    handle,
    noteNewerUserScroll: () => {
      userScrollGeneration += 1;
    },
    ref: { current: handle } satisfies VirtualizedConversationMessageListHandleRef,
  };
}

function installAnimationFrameHarness() {
  const callbacks = new Map<number, FrameRequestCallback>();
  let nextId = 1;
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
    const id = nextId;
    nextId += 1;
    callbacks.set(id, callback);
    return id;
  });
  vi.spyOn(window, "cancelAnimationFrame").mockImplementation((id) => {
    callbacks.delete(id);
  });
  return {
    flush: () => {
      const pending = [...callbacks.values()];
      callbacks.clear();
      for (const callback of pending) {
        callback(performance.now());
      }
    },
  };
}

describe("useResponseBoardSourceNavigation", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("jumps after the accepted history window is adopted", async () => {
    const frame = installAnimationFrameHarness();
    const historyRequest = deferred<boolean>();
    const requestHistoryAround = vi.fn(() => historyRequest.promise);
    const jumpToMessageId = vi.fn();
    const virtualizer = makeVirtualizerHandle();
    renderHook(() =>
      useResponseBoardSourceNavigation({
        jumpToMessageId,
        requestHistoryAround,
        sessionId: "session-success",
        subscriberKey: {},
        virtualizerHandleRef: virtualizer.ref,
      }),
    );

    act(() => {
      requestResponseBoardSourceNavigation({
        messageId: "message-42",
        messagePosition: 42,
        sessionId: "session-success",
      });
    });
    expect(
      virtualizer.handle.beginUserScrollNavigation,
    ).toHaveBeenCalledOnce();
    expect(requestHistoryAround).toHaveBeenCalledWith(42);

    await act(async () => {
      historyRequest.resolve(true);
      await historyRequest.promise;
    });
    act(() => frame.flush());

    expect(jumpToMessageId).toHaveBeenCalledOnce();
    expect(jumpToMessageId).toHaveBeenCalledWith("message-42");
  });

  it("jumps through the mounted DOM when the transcript is not virtualized", async () => {
    const frame = installAnimationFrameHarness();
    const historyRequest = deferred<boolean>();
    const requestHistoryAround = vi.fn(() => historyRequest.promise);
    const jumpToMessageId = vi.fn();
    const virtualizerHandleRef: VirtualizedConversationMessageListHandleRef = {
      current: null,
    };
    renderHook(() =>
      useResponseBoardSourceNavigation({
        jumpToMessageId,
        requestHistoryAround,
        sessionId: "session-short",
        subscriberKey: {},
        virtualizerHandleRef,
      }),
    );

    act(() => {
      requestResponseBoardSourceNavigation({
        messageId: "message-visible",
        messagePosition: 3,
        sessionId: "session-short",
      });
    });
    expect(requestHistoryAround).toHaveBeenCalledWith(3);

    await act(async () => {
      historyRequest.resolve(true);
      await historyRequest.promise;
    });
    act(() => frame.flush());

    expect(jumpToMessageId).toHaveBeenCalledOnce();
    expect(jumpToMessageId).toHaveBeenCalledWith("message-visible");
  });

  it("cancels when a newly mounted virtualizer has newer scroll input", async () => {
    const frame = installAnimationFrameHarness();
    const historyRequest = deferred<boolean>();
    const jumpToMessageId = vi.fn();
    const virtualizer = makeVirtualizerHandle();
    const virtualizerHandleRef: VirtualizedConversationMessageListHandleRef = {
      current: null,
    };
    renderHook(() =>
      useResponseBoardSourceNavigation({
        jumpToMessageId,
        requestHistoryAround: () => historyRequest.promise,
        sessionId: "session-growing",
        subscriberKey: {},
        virtualizerHandleRef,
      }),
    );

    act(() => {
      requestResponseBoardSourceNavigation({
        messageId: "message-growing",
        messagePosition: 35,
        sessionId: "session-growing",
      });
    });
    virtualizerHandleRef.current = virtualizer.handle;
    virtualizer.noteNewerUserScroll();
    await act(async () => {
      historyRequest.resolve(true);
      await historyRequest.promise;
    });
    act(() => frame.flush());

    expect(virtualizer.handle.beginUserScrollNavigation).not.toHaveBeenCalled();
    expect(jumpToMessageId).not.toHaveBeenCalled();
  });

  it("joins a newly mounted virtualizer guard before jumping", async () => {
    const frame = installAnimationFrameHarness();
    const historyRequest = deferred<boolean>();
    const jumpToMessageId = vi.fn();
    const virtualizer = makeVirtualizerHandle();
    const virtualizerHandleRef: VirtualizedConversationMessageListHandleRef = {
      current: null,
    };
    renderHook(() =>
      useResponseBoardSourceNavigation({
        jumpToMessageId,
        requestHistoryAround: () => historyRequest.promise,
        sessionId: "session-mounted",
        subscriberKey: {},
        virtualizerHandleRef,
      }),
    );

    act(() => {
      requestResponseBoardSourceNavigation({
        messageId: "message-mounted",
        messagePosition: 35,
        sessionId: "session-mounted",
      });
    });
    virtualizerHandleRef.current = virtualizer.handle;
    await act(async () => {
      historyRequest.resolve(true);
      await historyRequest.promise;
    });
    act(() => frame.flush());

    expect(virtualizer.handle.beginUserScrollNavigation).toHaveBeenCalledOnce();
    expect(jumpToMessageId).toHaveBeenCalledWith("message-mounted");
  });

  it("reports a failed history request without scheduling a jump", async () => {
    const frame = installAnimationFrameHarness();
    const historyRequest = deferred<boolean>();
    const jumpToMessageId = vi.fn();
    const virtualizer = makeVirtualizerHandle();
    const requestError = new Error("history unavailable");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    renderHook(() =>
      useResponseBoardSourceNavigation({
        jumpToMessageId,
        requestHistoryAround: () => historyRequest.promise,
        sessionId: "session-error",
        subscriberKey: {},
        virtualizerHandleRef: virtualizer.ref,
      }),
    );

    act(() => {
      requestResponseBoardSourceNavigation({
        messageId: "message-error",
        messagePosition: 9,
        sessionId: "session-error",
      });
    });
    await act(async () => {
      historyRequest.reject(requestError);
      await historyRequest.promise.catch(() => {});
    });
    act(() => frame.flush());

    expect(jumpToMessageId).not.toHaveBeenCalled();
    expect(warn).toHaveBeenCalledWith(
      "Response-board source navigation failed to load history.",
      {
        error: requestError,
        messageId: "message-error",
        sessionId: "session-error",
      },
    );
  });

  it("cancels the delayed jump after newer user scroll input", async () => {
    const frame = installAnimationFrameHarness();
    const historyRequest = deferred<boolean>();
    const jumpToMessageId = vi.fn();
    const virtualizer = makeVirtualizerHandle();
    renderHook(() =>
      useResponseBoardSourceNavigation({
        jumpToMessageId,
        requestHistoryAround: () => historyRequest.promise,
        sessionId: "session-scroll",
        subscriberKey: {},
        virtualizerHandleRef: virtualizer.ref,
      }),
    );

    act(() => {
      requestResponseBoardSourceNavigation({
        messageId: "message-old",
        messagePosition: 10,
        sessionId: "session-scroll",
      });
    });
    await act(async () => {
      historyRequest.resolve(true);
      await historyRequest.promise;
    });
    virtualizer.noteNewerUserScroll();
    act(() => frame.flush());

    expect(jumpToMessageId).not.toHaveBeenCalled();
  });

  it("cancels the delayed jump when the pane switches sessions", async () => {
    const frame = installAnimationFrameHarness();
    const historyRequest = deferred<boolean>();
    const jumpToMessageId = vi.fn();
    const virtualizer = makeVirtualizerHandle();
    const subscriberKey = {};
    const { rerender } = renderHook(
      ({ sessionId }: { sessionId: string }) =>
        useResponseBoardSourceNavigation({
          jumpToMessageId,
          requestHistoryAround: () => historyRequest.promise,
          sessionId,
          subscriberKey,
          virtualizerHandleRef: virtualizer.ref,
        }),
      { initialProps: { sessionId: "session-before" } },
    );

    act(() => {
      requestResponseBoardSourceNavigation({
        messageId: "message-before",
        messagePosition: 8,
        sessionId: "session-before",
      });
    });
    await act(async () => {
      historyRequest.resolve(true);
      await historyRequest.promise;
    });
    rerender({ sessionId: "session-after" });
    act(() => frame.flush());

    expect(jumpToMessageId).not.toHaveBeenCalled();
  });
});
