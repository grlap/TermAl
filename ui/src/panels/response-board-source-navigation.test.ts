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
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
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
    expect(virtualizer.handle.beginUserScrollNavigation).toHaveBeenCalledOnce();
    expect(requestHistoryAround).toHaveBeenCalledWith(42);

    await act(async () => {
      historyRequest.resolve(true);
      await historyRequest.promise;
    });
    act(() => frame.flush());

    expect(jumpToMessageId).toHaveBeenCalledOnce();
    expect(jumpToMessageId).toHaveBeenCalledWith("message-42");
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
