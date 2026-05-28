import { describe, expect, it } from "vitest";

import {
  classifyFetchedSessionAdoption,
  hydrationRetainedMessagesMatch,
  hydrationSessionMetadataIsAhead,
  hydrationSessionMetadataMatches,
  type SessionHydrationRequestContext,
} from "./session-hydration-adoption";
import type { Message, Session } from "./types";

function makeSession(overrides: Partial<Session> = {}): Session {
  return {
    id: "session-1",
    name: "Session",
    emoji: "AI",
    agent: "Codex",
    workdir: "C:/workspace",
    model: "codex",
    status: "idle",
    preview: "",
    messages: [],
    messagesLoaded: false,
    ...overrides,
  };
}

function makeHydrationRequestContext(
  overrides: Partial<SessionHydrationRequestContext> = {},
): SessionHydrationRequestContext {
  return {
    kind: "fullSession",
    messageCount: 1,
    revision: 5,
    serverInstanceId: "server-a",
    sessionMutationStamp: 1,
    ...overrides,
  };
}

const message: Message = {
  id: "message-1",
  type: "text",
  author: "assistant",
  timestamp: "10:00",
  text: "Hello",
};

describe("classifyFetchedSessionAdoption", () => {
  it("adopts a matching loaded same-instance response", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        responseRevision: 5,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext(),
        currentSession: makeSession({
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        currentRevision: 5,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("adopted");
  });

  it("classifies replacement-instance hydration as restart resync", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        responseRevision: 1,
        responseServerInstanceId: "server-b",
        requestContext: makeHydrationRequestContext(),
        currentSession: makeSession({
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        currentRevision: 5,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("restartResync");
  });

  it("requests a state resync when fetched metadata is ahead of the summary", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message, { ...message, id: "message-2", text: "Newer" }],
          messagesLoaded: true,
          messageCount: 2,
          sessionMutationStamp: 2,
        }),
        responseRevision: 5,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext(),
        currentSession: makeSession({
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        currentRevision: 5,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("stateResync");
  });

  it("adopts a loaded response when retained text diverged but metadata still matches", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        responseRevision: 6,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext({
          revision: 6,
          sessionMutationStamp: 2,
        }),
        currentSession: makeSession({
          messages: [{ ...message, text: "Corrupted live stream" }],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        currentRevision: 6,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("adopted");
  });

  it("adopts current loaded responses without serializing retained messages", () => {
    const retainedMessage = {
      ...message,
      text: "Client retained text can be stale on a current hydration.",
      toJSON: () => {
        throw new Error("retained message should not be serialized");
      },
    } as unknown as Message;

    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        responseRevision: 6,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext({
          revision: 6,
          sessionMutationStamp: 2,
        }),
        currentSession: makeSession({
          messages: [retainedMessage],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        currentRevision: 6,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("adopted");
  });

  it("rejects divergent same-metadata hydration after a newer live revision", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        responseRevision: 7,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext({
          revision: 6,
          sessionMutationStamp: 2,
        }),
        currentSession: makeSession({
          messages: [{ ...message, text: "Newer live stream" }],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        currentRevision: 7,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("stale");
  });

  it("allows explicit text-repair hydration after an unrelated newer live revision", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        responseRevision: 7,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext({
          kind: "textRepair",
          revision: 6,
          sessionMutationStamp: 2,
        }),
        currentSession: makeSession({
          messages: [{ ...message, text: "Corrupted gapped live stream" }],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        currentRevision: 7,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("adopted");
  });

  it("allows explicit text-repair hydration at the request revision after an unrelated newer live revision", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        responseRevision: 6,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext({
          kind: "textRepair",
          revision: 6,
          sessionMutationStamp: 2,
        }),
        currentSession: makeSession({
          messages: [{ ...message, text: "Corrupted gapped live stream" }],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 2,
        }),
        currentRevision: 7,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("adopted");
  });

  it("allows partial transcript adoption only for tail hydration requests", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: false,
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        responseRevision: 5,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext({ kind: "partialTail" }),
        currentSession: makeSession({
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        currentRevision: 5,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("partial");
  });

  it("rejects stale lower-revision responses once the session is loaded", () => {
    expect(
      classifyFetchedSessionAdoption({
        responseSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        responseRevision: 9,
        responseServerInstanceId: "server-a",
        requestContext: makeHydrationRequestContext({ revision: 9 }),
        currentSession: makeSession({
          messages: [message],
          messagesLoaded: true,
          messageCount: 1,
          sessionMutationStamp: 1,
        }),
        currentRevision: 10,
        currentServerInstanceId: "server-a",
        seenServerInstanceIds: new Set(["server-a"]),
      }),
    ).toBe("stale");
  });
});

describe("hydrationRetainedMessagesMatch", () => {
  it("matches structurally identical retained messages", () => {
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [{ ...message }] },
        { messages: [{ ...message }] },
      ),
    ).toBe(true);
  });

  it("matches retained messages that appear as an ordered subsequence of the hydrated transcript", () => {
    const olderMessage: Message = {
      ...message,
      id: "message-older",
      text: "Older retained message",
    };
    const missingMessage: Message = {
      ...message,
      id: "message-missing",
      text: "Message only present after hydration",
    };
    const latestMessage: Message = {
      ...message,
      id: "message-latest",
      text: "Latest retained message",
    };

    expect(
      hydrationRetainedMessagesMatch(
        { messages: [olderMessage, missingMessage, latestMessage] },
        { messages: [olderMessage, latestMessage] },
      ),
    ).toBe(true);
  });

  it("treats either empty side as retainable", () => {
    expect(
      hydrationRetainedMessagesMatch({ messages: [] }, { messages: [message] }),
    ).toBe(true);
    expect(
      hydrationRetainedMessagesMatch({ messages: [message] }, { messages: [] }),
    ).toBe(true);
  });

  it("rejects non-empty message shape mismatches", () => {
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [{ ...message, text: "Hello" }] },
        { messages: [{ ...message, text: "Goodbye" }] },
      ),
    ).toBe(false);
  });

  it("rejects retained messages that are missing from the hydrated transcript", () => {
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [{ ...message, id: "message-other" }] },
        { messages: [message] },
      ),
    ).toBe(false);
  });

  it("rejects retained messages that appear out of order in the hydrated transcript", () => {
    const firstMessage: Message = {
      ...message,
      id: "message-first",
      text: "First",
    };
    const secondMessage: Message = {
      ...message,
      id: "message-second",
      text: "Second",
    };

    expect(
      hydrationRetainedMessagesMatch(
        { messages: [secondMessage, firstMessage] },
        { messages: [firstMessage, secondMessage] },
      ),
    ).toBe(false);
  });

  it("rejects extra client-side fields on retained messages", () => {
    expect(
      hydrationRetainedMessagesMatch(
        { messages: [message] },
        { messages: [{ ...message, localRenderCache: true } as Message] },
      ),
    ).toBe(false);
  });
});

describe("hydrationSessionMetadataMatches", () => {
  const baseSession = makeSession();

  it("rejects numeric response metadata when the current session captured null", () => {
    expect(
      hydrationSessionMetadataMatches(
        {
          ...baseSession,
          messageCount: 3,
          sessionMutationStamp: 7,
        },
        {
          ...baseSession,
          messageCount: null,
          sessionMutationStamp: null,
        },
      ),
    ).toBe(false);
  });

  it("treats null metadata as an exact value rather than a wildcard", () => {
    expect(
      hydrationSessionMetadataMatches(
        {
          ...baseSession,
          messageCount: null,
          sessionMutationStamp: null,
        },
        {
          ...baseSession,
          messageCount: null,
          sessionMutationStamp: null,
        },
      ),
    ).toBe(true);
  });

  it("falls back to loaded message length when messageCount is absent", () => {
    expect(
      hydrationSessionMetadataMatches(
        {
          ...baseSession,
          messages: [
            {
              id: "m1",
              type: "text",
              author: "assistant",
              timestamp: "10:00",
              text: "One",
            },
          ],
          messagesLoaded: true,
          sessionMutationStamp: 1,
        },
        {
          ...baseSession,
          messageCount: 1,
          messages: [],
          messagesLoaded: false,
          sessionMutationStamp: 1,
        },
      ),
    ).toBe(true);
  });
});

describe("hydrationSessionMetadataIsAhead", () => {
  const baseSession = makeSession();

  it("treats equal counts with a newer mutation stamp as ahead", () => {
    expect(
      hydrationSessionMetadataIsAhead(
        { ...baseSession, messageCount: 3, sessionMutationStamp: 11 },
        { ...baseSession, messageCount: 3, sessionMutationStamp: 10 },
      ),
    ).toBe(true);
  });

  it("does not treat equal counts with an equal mutation stamp as ahead", () => {
    expect(
      hydrationSessionMetadataIsAhead(
        { ...baseSession, messageCount: 3, sessionMutationStamp: 10 },
        { ...baseSession, messageCount: 3, sessionMutationStamp: 10 },
      ),
    ).toBe(false);
  });

  it("falls back to mutation stamps when message counts are unavailable", () => {
    expect(
      hydrationSessionMetadataIsAhead(
        { ...baseSession, messageCount: null, sessionMutationStamp: 2 },
        { ...baseSession, messageCount: null, sessionMutationStamp: 1 },
      ),
    ).toBe(true);
  });
});
