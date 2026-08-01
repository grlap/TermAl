import { afterEach, describe, expect, it, vi } from "vitest";

import {
  RESPONSE_BOARD_SOURCE_NAVIGATION_TTL_MS,
  requestResponseBoardSourceNavigation,
  subscribeResponseBoardSourceNavigation,
} from "./response-board-navigation";

describe("response-board source navigation", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("replays one pending around-position jump after the session pane mounts", async () => {
    const listener = vi.fn();
    requestResponseBoardSourceNavigation({
      sessionId: "session-1",
      messageId: "message-42",
      messagePosition: 42,
    });

    const unsubscribe = subscribeResponseBoardSourceNavigation(
      "session-1",
      listener,
    );
    await Promise.resolve();
    expect(listener).toHaveBeenCalledOnce();
    expect(listener).toHaveBeenCalledWith({
      sessionId: "session-1",
      messageId: "message-42",
      messagePosition: 42,
    });
    unsubscribe();
  });

  it("fans a pending jump out to every matching pane exactly once", async () => {
    const first = vi.fn();
    const second = vi.fn();
    const otherSession = vi.fn();
    const unsubscribeFirst = subscribeResponseBoardSourceNavigation(
      "session-fanout",
      first,
    );
    const unsubscribeOther = subscribeResponseBoardSourceNavigation(
      "session-other",
      otherSession,
    );

    const request = {
      sessionId: "session-fanout",
      messageId: "message-7",
      messagePosition: 7,
    };
    requestResponseBoardSourceNavigation(request);
    const unsubscribeSecond = subscribeResponseBoardSourceNavigation(
      "session-fanout",
      second,
    );
    await Promise.resolve();

    expect(first).toHaveBeenCalledOnce();
    expect(first).toHaveBeenCalledWith(request);
    expect(second).toHaveBeenCalledOnce();
    expect(second).toHaveBeenCalledWith(request);
    expect(otherSession).not.toHaveBeenCalled();

    unsubscribeFirst();
    unsubscribeSecond();
    unsubscribeOther();
  });

  it("does not replay a retained jump when one pane replaces its callback", async () => {
    const subscriberKey = {};
    const first = vi.fn();
    const unsubscribeFirst = subscribeResponseBoardSourceNavigation(
      "session-rerender",
      first,
      subscriberKey,
    );
    requestResponseBoardSourceNavigation({
      sessionId: "session-rerender",
      messageId: "message-current",
      messagePosition: 12,
    });
    expect(first).toHaveBeenCalledOnce();
    unsubscribeFirst();

    const replacement = vi.fn();
    const unsubscribeReplacement = subscribeResponseBoardSourceNavigation(
      "session-rerender",
      replacement,
      subscriberKey,
    );
    await Promise.resolve();

    expect(replacement).not.toHaveBeenCalled();
    unsubscribeReplacement();
  });

  it("expires an undelivered jump instead of replaying it indefinitely", async () => {
    vi.useFakeTimers();
    requestResponseBoardSourceNavigation({
      sessionId: "session-expired",
      messageId: "message-old",
      messagePosition: 3,
    });
    await vi.advanceTimersByTimeAsync(
      RESPONSE_BOARD_SOURCE_NAVIGATION_TTL_MS,
    );

    const listener = vi.fn();
    const unsubscribe = subscribeResponseBoardSourceNavigation(
      "session-expired",
      listener,
    );
    await Promise.resolve();

    expect(listener).not.toHaveBeenCalled();
    unsubscribe();
  });
});
