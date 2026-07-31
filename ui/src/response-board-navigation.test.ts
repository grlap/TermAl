import { describe, expect, it, vi } from "vitest";

import {
  requestResponseBoardSourceNavigation,
  subscribeResponseBoardSourceNavigation,
} from "./response-board-navigation";

describe("response-board source navigation", () => {
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
});
