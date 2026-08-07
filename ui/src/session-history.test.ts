import { describe, expect, it } from "vitest";

import {
  appendSessionHistoryPage,
  prependSessionHistoryPage,
  repairSessionTailFromHistoryPage,
  replaceSessionWithHistoryAroundPage,
  replaceSessionWithHistoryTailPage,
  replaceSessionWithHistoryStartPage,
} from "./session-history";
import type { Message, Session } from "./types";

type TextMessage = Extract<Message, { type: "text" }>;

function message(index: number): TextMessage {
  return {
    type: "text",
    id: `message-${index}`,
    timestamp: "12:00",
    author: "assistant",
    text: `Message ${index}`,
  };
}

function session(messages: Message[], options: Partial<Session> = {}): Session {
  return {
    id: "session-1",
    name: "History",
    emoji: "H",
    agent: "Codex",
    workdir: "/tmp",
    model: "gpt",
    status: "idle",
    preview: "",
    messages,
    messageCount: 1_000,
    messagesLoaded: false,
    sessionMutationStamp: 7,
    ...options,
  };
}

describe("session history page merging", () => {
  it("prepends an exclusive page while preserving a live-appended tail", () => {
    const current = session(
      [message(900), message(901), message(999), message(1_000)],
      {
        messageCount: 1_001,
      },
    );
    const outcome = prependSessionHistoryPage({
      current,
      requestedBefore: "message-900",
      page: {
        messages: [message(898), message(899)],
        nextBefore: "message-898",
        hasMore: true,
        messageCount: 1_000,
        revision: 10,
        sessionMutationStamp: 6,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind !== "applied") {
      return;
    }
    expect(outcome.session.messages.map((entry) => entry.id)).toEqual([
      "message-898",
      "message-899",
      "message-900",
      "message-901",
      "message-999",
      "message-1000",
    ]);
    expect(outcome.session.messageCount).toBe(1_001);
    expect(outcome.session.messagesLoaded).toBe(false);
  });

  it("marks the transcript loaded only when the page reaches the beginning", () => {
    const outcome = prependSessionHistoryPage({
      current: session([message(2), message(3)]),
      requestedBefore: "message-2",
      page: {
        messages: [message(0), message(1)],
        hasMore: false,
        messageCount: 4,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.messagesLoaded).toBe(true);
      expect(outcome.session.messages.map((entry) => entry.id)).toEqual([
        "message-0",
        "message-1",
        "message-2",
        "message-3",
      ]);
    }
  });

  it("keeps a detached window partial after prepending through the true start", () => {
    const outcome = prependSessionHistoryPage({
      current: session([message(2), message(3)], {
        hasNewerHistory: true,
      }),
      requestedBefore: "message-2",
      page: {
        messages: [message(0), message(1)],
        hasMore: false,
        messageCount: 100,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.hasOlderHistory).toBe(false);
      expect(outcome.session.hasNewerHistory).toBe(true);
      expect(outcome.session.messagesLoaded).toBe(false);
    }
  });

  it("rejects a delayed page after another prepend changes the cursor", () => {
    const outcome = prependSessionHistoryPage({
      current: session([message(800), message(900)]),
      requestedBefore: "message-900",
      page: {
        messages: [message(898), message(899)],
        nextBefore: "message-898",
        hasMore: true,
        messageCount: 1_000,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome).toEqual({ kind: "cursorChanged" });
  });

  it("repairs the latest bounded tail without discarding loaded older pages", () => {
    const current = session(
      [
        message(0),
        message(1),
        message(2),
        {
          ...message(3),
          text: "stale",
        },
      ],
      {
        messageCount: 4,
        messagesLoaded: true,
      },
    );
    const outcome = repairSessionTailFromHistoryPage({
      current,
      page: {
        messages: [message(2), message(3)],
        hasMore: true,
        nextBefore: "message-2",
        messageCount: 4,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.messages).toEqual([
        message(0),
        message(1),
        message(2),
        message(3),
      ]);
      expect(outcome.session.messagesLoaded).toBe(true);
    }
  });

  it("repairs a divergent retained suffix by authoritative message positions", () => {
    const current = session([message(0), message(1), message(2), message(4)], {
      messageCount: 5,
      messagesLoaded: true,
    });
    const outcome = repairSessionTailFromHistoryPage({
      current,
      page: {
        messages: [message(2), message(3), message(4)],
        hasMore: true,
        nextBefore: "message-2",
        messageStartIndex: 2,
        messageCount: 5,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.messages).toEqual([
        message(0),
        message(1),
        message(2),
        message(3),
        message(4),
      ]);
      expect(outcome.session.messagesLoaded).toBe(true);
      expect(outcome.session.messageStartIndex).toBe(0);
    }
  });

  it("repairs a divergent partial window without changing its global start", () => {
    const current = session([message(100), message(101), message(103)], {
      hasOlderHistory: true,
      messageCount: 104,
      messageStartIndex: 100,
    });
    const outcome = repairSessionTailFromHistoryPage({
      current,
      page: {
        messages: [message(101), message(102), message(103)],
        hasMore: true,
        nextBefore: "message-101",
        messageStartIndex: 101,
        messageCount: 104,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.messages).toEqual([
        message(100),
        message(101),
        message(102),
        message(103),
      ]);
      expect(outcome.session.messagesLoaded).toBe(false);
      expect(outcome.session.hasOlderHistory).toBe(true);
      expect(outcome.session.hasNewerHistory).toBe(false);
      expect(outcome.session.messageStartIndex).toBe(100);
    }
  });

  it("drops an untrusted retained prefix that duplicates the authoritative page", () => {
    const outcome = repairSessionTailFromHistoryPage({
      current: session(
        [message(0), message(2), message(90), message(91), message(92)],
        {
          messageCount: 5,
          messagesLoaded: true,
        },
      ),
      page: {
        messages: [message(2), message(3), message(4)],
        hasMore: true,
        nextBefore: "message-2",
        messageStartIndex: 2,
        messageCount: 5,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.messages).toEqual([
        message(2),
        message(3),
        message(4),
      ]);
      expect(outcome.session.messagesLoaded).toBe(false);
      expect(outcome.session.hasOlderHistory).toBe(true);
      expect(outcome.session.hasNewerHistory).toBe(false);
      expect(outcome.session.messageStartIndex).toBe(2);
    }
  });

  it("drops a retained prefix when the resident window extends past the transcript", () => {
    const outcome = repairSessionTailFromHistoryPage({
      current: session(
        [
          message(0),
          message(90),
          message(1),
          message(2),
          message(3),
          message(4),
        ],
        {
          messageCount: 5,
          messagesLoaded: true,
        },
      ),
      page: {
        messages: [message(2), message(3), message(4)],
        hasMore: true,
        nextBefore: "message-2",
        messageStartIndex: 2,
        messageCount: 5,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.messages).toEqual([
        message(2),
        message(3),
        message(4),
      ]);
      expect(outcome.session.messagesLoaded).toBe(false);
      expect(outcome.session.hasOlderHistory).toBe(true);
      expect(outcome.session.messageStartIndex).toBe(2);
    }
  });

  it("uses an authoritative page that starts before the resident window", () => {
    const outcome = repairSessionTailFromHistoryPage({
      current: session([message(3), message(4)], {
        hasOlderHistory: true,
        messageCount: 5,
        messageStartIndex: 3,
      }),
      page: {
        messages: [message(2), message(3), message(4)],
        hasMore: true,
        nextBefore: "message-2",
        messageStartIndex: 2,
        messageCount: 5,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.messages).toEqual([
        message(2),
        message(3),
        message(4),
      ]);
      expect(outcome.session.messagesLoaded).toBe(false);
      expect(outcome.session.hasOlderHistory).toBe(true);
      expect(outcome.session.hasNewerHistory).toBe(false);
      expect(outcome.session.messageStartIndex).toBe(2);
    }
  });

  it("requests a resync when the repair page is separated by a position gap", () => {
    const outcome = repairSessionTailFromHistoryPage({
      current: session([message(0), message(1)], {
        messageCount: 5,
        messagesLoaded: false,
        messageStartIndex: 0,
      }),
      page: {
        messages: [message(3), message(4)],
        hasMore: true,
        nextBefore: "message-3",
        messageStartIndex: 3,
        messageCount: 5,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome).toEqual({ kind: "cursorChanged" });
  });

  it("rejects a repair page that reports newer messages", () => {
    const outcome = repairSessionTailFromHistoryPage({
      current: session([message(0), message(1), message(2)], {
        messageCount: 3,
        messagesLoaded: true,
      }),
      page: {
        messages: [message(1), message(2)],
        hasMore: true,
        nextBefore: "message-1",
        hasNewer: true,
        nextAfter: "message-2",
        messageStartIndex: 1,
        messageCount: 3,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome).toEqual({
      kind: "protocolError",
      message:
        "session history repair page did not end at the live transcript tail",
    });
  });

  it("rejects a repair page whose positions stop before the live tail", () => {
    const outcome = repairSessionTailFromHistoryPage({
      current: session([message(0), message(1), message(2), message(3)], {
        messageCount: 4,
        messagesLoaded: true,
      }),
      page: {
        messages: [message(1), message(2)],
        hasMore: true,
        nextBefore: "message-1",
        messageStartIndex: 1,
        messageCount: 4,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome).toEqual({
      kind: "protocolError",
      message:
        "session history repair page did not end at the live transcript tail",
    });
  });

  it("rejects an empty repair page for a non-empty transcript", () => {
    const outcome = repairSessionTailFromHistoryPage({
      current: session([message(0)], {
        messageCount: 1,
        messagesLoaded: true,
      }),
      page: {
        messages: [],
        hasMore: false,
        messageStartIndex: 1,
        messageCount: 1,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome).toEqual({
      kind: "protocolError",
      message:
        "session history repair page was empty for a non-empty transcript",
    });
  });

  it("clears stale residency metadata for an authoritative empty transcript", () => {
    const outcome = repairSessionTailFromHistoryPage({
      current: session([message(0)], {
        hasOlderHistory: true,
        hasNewerHistory: true,
        messageCount: 0,
        messageStartIndex: 12,
      }),
      page: {
        messages: [],
        hasMore: false,
        messageStartIndex: 0,
        messageCount: 0,
        revision: 10,
        sessionMutationStamp: 7,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.messages).toEqual([]);
      expect(outcome.session.messagesLoaded).toBe(true);
      expect(outcome.session.hasOlderHistory).toBe(false);
      expect(outcome.session.hasNewerHistory).toBe(false);
      expect(outcome.session.messageStartIndex).toBe(0);
      expect(outcome.session.messageCount).toBe(0);
    }
  });

  it("replaces a live tail with one bounded true-start page", () => {
    const startMessages = Array.from({ length: 64 }, (_, index) =>
      message(index + 1),
    );
    const outcome = replaceSessionWithHistoryStartPage({
      current: session([message(981), message(1_000)], {
        messageCount: 20_001,
      }),
      page: {
        messages: startMessages,
        hasMore: false,
        hasNewer: true,
        nextAfter: "message-64",
        messageCount: 20_001,
        revision: 10,
        sessionMutationStamp: 8,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.messages).toEqual(startMessages);
      expect(outcome.session.hasOlderHistory).toBe(false);
      expect(outcome.session.hasNewerHistory).toBe(true);
      expect(outcome.session.messagesLoaded).toBe(false);
      expect(outcome.session.messageCount).toBe(20_001);
    }
  });

  it("keeps a stale true-start response detached from a concurrently newer tail", () => {
    const outcome = replaceSessionWithHistoryStartPage({
      current: session([message(1), message(2)], {
        messageCount: 2,
        sessionMutationStamp: 9,
      }),
      page: {
        messages: [message(1)],
        hasMore: false,
        hasNewer: false,
        messageCount: 1,
        revision: 10,
        sessionMutationStamp: 8,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.messages).toEqual([message(1)]);
      expect(outcome.session.hasNewerHistory).toBe(true);
      expect(outcome.session.messagesLoaded).toBe(false);
      expect(outcome.session.messageCount).toBe(2);
      expect(outcome.session.sessionMutationStamp).toBe(9);
    }
  });

  it("replaces the resident window with one centered position page", () => {
    const centeredMessages = Array.from({ length: 20 }, (_, index) =>
      message(index + 40),
    );
    const outcome = replaceSessionWithHistoryAroundPage({
      current: session([message(900), message(901)]),
      requestedPosition: 50,
      page: {
        messages: centeredMessages,
        nextBefore: "message-40",
        hasMore: true,
        nextAfter: "message-59",
        hasNewer: true,
        messageStartIndex: 40,
        messageCount: 1_000,
        revision: 10,
        sessionMutationStamp: 8,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.messages).toEqual(centeredMessages);
      expect(outcome.session.messageStartIndex).toBe(40);
      expect(outcome.session.hasOlderHistory).toBe(true);
      expect(outcome.session.hasNewerHistory).toBe(true);
      expect(outcome.session.messagesLoaded).toBe(false);
    }
  });

  it("rejects an around page that does not contain the requested position", () => {
    const outcome = replaceSessionWithHistoryAroundPage({
      current: session([message(900), message(901)]),
      requestedPosition: 50,
      page: {
        messages: [message(60), message(61)],
        nextBefore: "message-60",
        hasMore: true,
        nextAfter: "message-61",
        hasNewer: true,
        messageStartIndex: 60,
        messageCount: 1_000,
        revision: 10,
        sessionMutationStamp: 8,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("protocolError");
  });

  it("appends exactly one bounded forward page after a true-start page", () => {
    const current = session(
      Array.from({ length: 64 }, (_, index) => message(index + 1)),
      {
        hasOlderHistory: false,
        hasNewerHistory: true,
        messageCount: 20_001,
      },
    );
    const nextMessages = [message(65), message(66)];
    const outcome = appendSessionHistoryPage({
      current,
      requestedAfter: "message-64",
      page: {
        messages: nextMessages,
        hasMore: true,
        nextBefore: "message-65",
        hasNewer: true,
        nextAfter: "message-66",
        messageCount: 20_001,
        revision: 11,
        sessionMutationStamp: 8,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.messages).toHaveLength(66);
      expect(
        outcome.session.messages[outcome.session.messages.length - 1]?.id,
      ).toBe("message-66");
      expect(outcome.session.hasOlderHistory).toBe(false);
      expect(outcome.session.hasNewerHistory).toBe(true);
      expect(outcome.session.messagesLoaded).toBe(false);
    }
  });

  it("replaces a historical window with one bounded live-tail page", () => {
    const tailMessages = Array.from({ length: 64 }, (_, index) =>
      message(937 + index),
    );
    const outcome = replaceSessionWithHistoryTailPage({
      current: session(
        Array.from({ length: 64 }, (_, index) => message(index + 1)),
        {
          hasOlderHistory: false,
          hasNewerHistory: true,
          messageCount: 1_000,
        },
      ),
      page: {
        messages: tailMessages,
        hasMore: true,
        nextBefore: "message-937",
        hasNewer: false,
        messageCount: 1_000,
        revision: 12,
        sessionMutationStamp: 8,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome.kind).toBe("applied");
    if (outcome.kind === "applied") {
      expect(outcome.session.messages).toEqual(tailMessages);
      expect(outcome.session.hasOlderHistory).toBe(true);
      expect(outcome.session.hasNewerHistory).toBe(false);
      expect(outcome.session.messagesLoaded).toBe(false);
    }
  });

  it("does not replace history with a tail page older than live metadata", () => {
    const outcome = replaceSessionWithHistoryTailPage({
      current: session([message(1)], {
        hasNewerHistory: true,
        messageCount: 1_001,
        sessionMutationStamp: 9,
      }),
      page: {
        messages: [message(999), message(1_000)],
        hasMore: true,
        nextBefore: "message-999",
        hasNewer: false,
        messageCount: 1_000,
        revision: 12,
        sessionMutationStamp: 8,
        serverInstanceId: "server-1",
      },
    });

    expect(outcome).toEqual({ kind: "metadataChanged" });
  });
});
