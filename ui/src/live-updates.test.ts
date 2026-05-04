import {
  LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS,
  applyDeltaToSessions,
  pruneLiveTransportActivitySessions,
  sessionDeltaAdvancesCurrentMutationStamp,
  sessionHasPotentiallyStaleTransport,
  type SessionDeltaEvent,
} from "./live-updates";
import type {
  CodexAppRequestMessage,
  DeltaEvent,
  McpElicitationRequestMessage,
  Session,
  UserInputRequestMessage,
} from "./types";

function makeSession(id: string, overrides?: Partial<Session>): Session {
  return {
    id,
    name: id,
    emoji: "x",
    agent: "Codex",
    workdir: "/tmp",
    model: "test-model",
    status: "idle",
    preview: "",
    messages: [],
    ...overrides,
  };
}

describe("sessionDeltaAdvancesCurrentMutationStamp", () => {
  const textDelta: SessionDeltaEvent = {
    type: "textDelta",
    revision: 4,
    sessionId: "session-1",
    messageId: "message-1",
    messageIndex: 0,
    messageCount: 1,
    delta: "ocused",
    sessionMutationStamp: 11,
  };

  it("accepts a delta whose session mutation stamp advances the local session", () => {
    expect(
      sessionDeltaAdvancesCurrentMutationStamp(
        [makeSession("session-1", { sessionMutationStamp: 10 })],
        textDelta,
      ),
    ).toBe(true);
    expect(
      sessionDeltaAdvancesCurrentMutationStamp(
        [makeSession("session-1", { sessionMutationStamp: 9 })],
        textDelta,
      ),
    ).toBe(true);
  });

  it("accepts a stamped delta when the retained session has no known stamp", () => {
    expect(
      sessionDeltaAdvancesCurrentMutationStamp(
        [makeSession("session-1", { sessionMutationStamp: null })],
        textDelta,
      ),
    ).toBe(true);
  });

  it("rejects missing or stale session mutation stamps", () => {
    expect(
      sessionDeltaAdvancesCurrentMutationStamp(
        [makeSession("session-1", { sessionMutationStamp: 10 })],
        { ...textDelta, sessionMutationStamp: null },
      ),
    ).toBe(false);
    expect(
      sessionDeltaAdvancesCurrentMutationStamp(
        [makeSession("session-1", { sessionMutationStamp: 11 })],
        textDelta,
      ),
    ).toBe(false);
  });

  it("rejects advancing stamps for unknown sessions", () => {
    expect(
      sessionDeltaAdvancesCurrentMutationStamp(
        [makeSession("session-2", { sessionMutationStamp: 10 })],
        textDelta,
      ),
    ).toBe(false);
  });
});

const resolvedInteractionBoundaryCases: {
  label: string;
  pending:
    | UserInputRequestMessage
    | McpElicitationRequestMessage
    | CodexAppRequestMessage;
  resolved:
    | UserInputRequestMessage
    | McpElicitationRequestMessage
    | CodexAppRequestMessage;
}[] = [
  {
    label: "user input request",
    pending: {
      id: "message-user-input-pending",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:02",
      title: "Need details",
      detail: "Please choose one option.",
      questions: [],
      state: "pending",
    },
    resolved: {
      id: "message-user-input-submitted",
      type: "userInputRequest",
      author: "assistant",
      timestamp: "10:02",
      title: "Need details",
      detail: "Please choose one option.",
      questions: [],
      state: "submitted",
      submittedAnswers: { answer: ["yes"] },
    },
  },
  {
    label: "MCP elicitation request",
    pending: {
      id: "message-mcp-pending",
      type: "mcpElicitationRequest",
      author: "assistant",
      timestamp: "10:02",
      title: "Need MCP input",
      detail: "Choose an option.",
      state: "pending",
      request: {
        threadId: "thread-1",
        serverName: "deployment-helper",
        mode: "form",
        message: "Choose an option",
        requestedSchema: {
          type: "object",
          properties: {
            choice: {
              type: "string",
              title: "Choice",
              enum: ["accept", "decline"],
            },
          },
          required: ["choice"],
        },
      },
    },
    resolved: {
      id: "message-mcp-submitted",
      type: "mcpElicitationRequest",
      author: "assistant",
      timestamp: "10:02",
      title: "Need MCP input",
      detail: "Choose an option.",
      state: "submitted",
      request: {
        threadId: "thread-1",
        serverName: "deployment-helper",
        mode: "form",
        message: "Choose an option",
        requestedSchema: {
          type: "object",
          properties: {
            choice: {
              type: "string",
              title: "Choice",
              enum: ["accept", "decline"],
            },
          },
          required: ["choice"],
        },
      },
      submittedAction: "accept",
      submittedContent: {
        choice: "accept",
      },
    },
  },
  {
    label: "Codex app request",
    pending: {
      id: "message-codex-app-pending",
      type: "codexAppRequest",
      author: "assistant",
      timestamp: "10:02",
      title: "App request",
      detail: "Return some JSON.",
      method: "workspace.pick_file",
      params: {
        allowMultiple: false,
      },
      state: "pending",
    },
    resolved: {
      id: "message-codex-app-submitted",
      type: "codexAppRequest",
      author: "assistant",
      timestamp: "10:02",
      title: "App request",
      detail: "Return some JSON.",
      method: "workspace.pick_file",
      params: {
        allowMultiple: false,
      },
      state: "submitted",
      submittedResult: {
        path: "/repo/src/main.ts",
      },
    },
  },
];

describe("session transport helpers", () => {
  it("does not treat active sessions without tracked transport activity as stale", () => {
    const session = makeSession("session-1", {
      status: "active",
      messages: [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "test",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Partial output.",
        },
      ],
    });

    expect(
      sessionHasPotentiallyStaleTransport(
        session,
        undefined,
        LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 1000,
      ),
    ).toBe(false);
  });

  it("treats a user prompt awaiting an assistant response as in-turn activity", () => {
    // A pending user prompt at the turn boundary is exactly the scenario
    // where the SSE delta carrying the assistant's first chunk could be
    // dropped silently — we want the watchdog to fire and resync rather
    // than leaving the user staring at "thinking" forever. See bugs.md
    // "Watchdog ignored user-prompt turn boundaries…".
    const session = makeSession("session-1", {
      status: "active",
      messages: [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "First prompt",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Earlier reply.",
        },
        {
          id: "message-user-2",
          type: "text",
          timestamp: "10:02",
          author: "you",
          text: "Latest prompt",
        },
      ],
    });

    expect(
      sessionHasPotentiallyStaleTransport(
        session,
        0,
        LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS,
      ),
    ).toBe(true);
  });

  it("treats a single pending user prompt with no prior assistant activity as in-turn activity", () => {
    // First-prompt-on-a-fresh-session scenario: the user just sent the very
    // first message in the session. If the assistant's first chunk delta is
    // lost, the watchdog must still fire after the staleness threshold so
    // the session can recover without the user re-sending or refreshing.
    const session = makeSession("session-1", {
      status: "active",
      messages: [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "First prompt with no reply yet",
        },
      ],
    });

    expect(
      sessionHasPotentiallyStaleTransport(
        session,
        0,
        LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS,
      ),
    ).toBe(true);
  });

  it("treats assistant output after the latest user turn as current-turn activity", () => {
    const session = makeSession("session-1", {
      status: "active",
      messages: [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Prompt",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Partial output.",
        },
      ],
    });

    expect(
      sessionHasPotentiallyStaleTransport(
        session,
        0,
        LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS,
      ),
    ).toBe(true);
  });

  it("does not treat active sessions just below the stale threshold as stale", () => {
    const session = makeSession("session-1", {
      status: "active",
      messages: [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "test",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Partial output.",
        },
      ],
    });

    expect(
      sessionHasPotentiallyStaleTransport(
        session,
        0,
        LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS - 1,
      ),
    ).toBe(false);
  });

  it("treats active sessions at the stale threshold as stale", () => {
    const session = makeSession("session-1", {
      status: "active",
      messages: [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "test",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Partial output.",
        },
      ],
    });

    expect(
      sessionHasPotentiallyStaleTransport(
        session,
        0,
        LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS,
      ),
    ).toBe(true);
  });

  it.each(resolvedInteractionBoundaryCases)(
    "treats assistant activity before a pending $label as current-turn activity",
    ({ pending }) => {
      const session = makeSession("session-1", {
        status: "active",
        messages: [
          {
            id: "message-user-1",
            type: "text",
            timestamp: "10:00",
            author: "you",
            text: "Prompt",
          },
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Earlier reply.",
          },
          pending,
        ],
      });

      expect(
        sessionHasPotentiallyStaleTransport(
          session,
          0,
          LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS,
        ),
      ).toBe(true);
    },
  );

  it.each(resolvedInteractionBoundaryCases)(
    "treats a resolved $label as a current-turn boundary",
    ({ resolved }) => {
      const session = makeSession("session-1", {
        status: "active",
        messages: [
          {
            id: "message-user-1",
            type: "text",
            timestamp: "10:00",
            author: "you",
            text: "Prompt",
          },
          {
            id: "message-assistant-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Earlier reply.",
          },
          resolved,
        ],
      });

      expect(
        sessionHasPotentiallyStaleTransport(
          session,
          0,
          LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS,
        ),
      ).toBe(false);
    },
  );

  it("prunes tracked transport activity for sessions missing from the latest snapshot", () => {
    const liveTransportActivityAtBySessionId = new Map<string, number>([
      ["session-1", 1000],
      ["session-2", 2000],
      ["session-3", 3000],
    ]);

    pruneLiveTransportActivitySessions(liveTransportActivityAtBySessionId, [
      makeSession("session-2"),
      makeSession("session-4"),
    ]);

    expect(Array.from(liveTransportActivityAtBySessionId.entries())).toEqual([
      ["session-2", 2000],
    ]);
  });

  it("prunes all tracked transport activity when the latest snapshot has no sessions", () => {
    const liveTransportActivityAtBySessionId = new Map<string, number>([
      ["session-1", 1000],
      ["session-2", 2000],
    ]);

    pruneLiveTransportActivitySessions(liveTransportActivityAtBySessionId, []);

    expect(Array.from(liveTransportActivityAtBySessionId.entries())).toEqual(
      [],
    );
  });

  it("does nothing when transport activity is already empty", () => {
    const liveTransportActivityAtBySessionId = new Map<string, number>();

    pruneLiveTransportActivitySessions(liveTransportActivityAtBySessionId, [
      makeSession("session-1"),
    ]);

    expect(Array.from(liveTransportActivityAtBySessionId.entries())).toEqual(
      [],
    );
  });
});

describe("applyDeltaToSessions", () => {
  it("preserves hydrated messages when a sessionCreated delta carries a summary", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: true,
        messageCount: 1,
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Hydrated transcript",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "sessionCreated",
      revision: 2,
      sessionId: "session-a",
      session: makeSession("session-a", {
        name: "Renamed session",
        preview: "Updated summary",
        messagesLoaded: false,
        messageCount: 1,
        messages: [],
        sessionMutationStamp: 202,
      }),
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }
    expect(result.sessions[0].name).toBe("Renamed session");
    expect(result.sessions[0].preview).toBe("Updated summary");
    expect(result.sessions[0].messagesLoaded).toBe(true);
    expect(result.sessions[0].messageCount).toBe(1);
    expect(result.sessions[0].sessionMutationStamp).toBe(202);
    expect(result.sessions[0].messages).toEqual(sessions[0].messages);
    expect(result.sessions[0].messages).toBe(sessions[0].messages);
    expect(result.sessions[0].messages[0]).toBe(sessions[0].messages[0]);
  });

  it("applies authoritative sessionCreated metadata even when the mutation stamp matches", () => {
    const sessions = [
      makeSession("session-a", {
        name: "Old session name",
        preview: "Old summary",
        messagesLoaded: true,
        messageCount: 1,
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Hydrated transcript",
          },
        ],
        sessionMutationStamp: 202,
      }),
    ];
    const delta: DeltaEvent = {
      type: "sessionCreated",
      revision: 2,
      sessionId: "session-a",
      session: makeSession("session-a", {
        name: "Authoritative session name",
        preview: "Authoritative summary",
        messagesLoaded: false,
        messageCount: 1,
        messages: [],
        sessionMutationStamp: 202,
      }),
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }
    expect(result.sessions[0].name).toBe("Authoritative session name");
    expect(result.sessions[0].preview).toBe("Authoritative summary");
    expect(result.sessions[0].messagesLoaded).toBe(true);
    expect(result.sessions[0].messages).toEqual(sessions[0].messages);
    expect(result.sessions[0].messages).toBe(sessions[0].messages);
    expect(result.sessions[0].messages[0]).toBe(sessions[0].messages[0]);
  });

  it("appends a created message without needing a resync", () => {
    const sessions = [makeSession("session-a")];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 1,
      sessionId: "session-a",
      messageId: "message-1",
      messageIndex: 0,
      messageCount: 1,
      message: {
        id: "message-1",
        type: "text",
        timestamp: "10:00",
        author: "assistant",
        text: "",
      },
      preview: "Waiting for activity.",
      status: "active",
      sessionMutationStamp: 101,
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].status).toBe("active");
    expect(result.sessions[0].messageCount).toBe(1);
    expect(result.sessions[0].sessionMutationStamp).toBe(101);
    expect(result.sessions[0].messages).toHaveLength(1);
    expect(result.sessions[0].messages[0]).toMatchObject({
      id: "message-1",
      type: "text",
      text: "",
    });
  });

  it("removes a queued prompt when its user message is created", () => {
    const sessions = [
      makeSession("session-a", {
        pendingPrompts: [
          {
            id: "message-queued-1",
            timestamp: "10:00",
            text: "Queued prompt",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 1,
      sessionId: "session-a",
      messageId: "message-queued-1",
      messageIndex: 0,
      messageCount: 1,
      message: {
        id: "message-queued-1",
        type: "text",
        timestamp: "10:00",
        author: "you",
        text: "Queued prompt",
      },
      preview: "Queued prompt",
      status: "active",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }
    expect(result.sessions[0].pendingPrompts).toBeUndefined();
    expect(result.sessions[0].messages.map((message) => message.id)).toEqual([
      "message-queued-1",
    ]);
  });

  it("retains created messages for unhydrated summaries without forcing a state resync", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 0,
        messages: [],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 1,
      sessionId: "session-a",
      messageId: "message-1",
      messageIndex: 1,
      messageCount: 2,
      message: {
        id: "message-1",
        type: "text",
        timestamp: "10:00",
        author: "assistant",
        text: "Visible before hydration",
      },
      preview: "Waiting for activity.",
      status: "active",
      sessionMutationStamp: 101,
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta metadata to apply");
    }

    expect(result.sessions[0].messagesLoaded).toBe(false);
    expect(result.sessions[0].messages.map((message) => message.id)).toEqual([
      "message-1",
    ]);
    expect(result.sessions[0].messages[0]).toMatchObject({
      author: "assistant",
      text: "Visible before hydration",
    });
    expect(result.sessions[0].status).toBe("active");
    expect(result.sessions[0].preview).toBe("Waiting for activity.");
    expect(result.sessions[0].messageCount).toBe(2);
    expect(result.sessions[0].sessionMutationStamp).toBe(101);
  });

  it("retains a new prompt delta when an unhydrated summary still has a contiguous transcript", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 1,
        sessionMutationStamp: 100,
        messages: [
          {
            id: "message-previous",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Previous answer",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 2,
      sessionId: "session-a",
      messageId: "message-new-prompt",
      messageIndex: 1,
      messageCount: 2,
      message: {
        id: "message-new-prompt",
        type: "text",
        timestamp: "10:01",
        author: "you",
        text: "Latest prompt",
      },
      preview: "Latest prompt",
      status: "active",
      sessionMutationStamp: 101,
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected prompt delta to apply");
    }

    expect(result.sessions[0].messagesLoaded).toBe(true);
    expect(result.sessions[0].messages.map((message) => message.id)).toEqual([
      "message-previous",
      "message-new-prompt",
    ]);
    expect(result.sessions[0].messages[1]).toMatchObject({
      author: "you",
      text: "Latest prompt",
    });
    expect(result.sessions[0].messageCount).toBe(2);
    expect(result.sessions[0].sessionMutationStamp).toBe(101);
  });

  it("retains a new prompt delta across an unhydrated transcript gap without marking it loaded", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 1,
        sessionMutationStamp: 100,
        messages: [
          {
            id: "message-previous",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Previous answer",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 2,
      sessionId: "session-a",
      messageId: "message-late-prompt",
      messageIndex: 3,
      messageCount: 4,
      message: {
        id: "message-late-prompt",
        type: "text",
        timestamp: "10:03",
        author: "you",
        text: "Prompt after missing messages",
      },
      preview: "Prompt after missing messages",
      status: "active",
      sessionMutationStamp: 101,
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected prompt delta to apply");
    }

    expect(result.sessions[0].messagesLoaded).toBe(false);
    expect(result.sessions[0].messages.map((message) => message.id)).toEqual([
      "message-previous",
      "message-late-prompt",
    ]);
    expect(result.sessions[0].messages[1]).toMatchObject({
      author: "you",
      text: "Prompt after missing messages",
    });
    expect(result.sessions[0].messageCount).toBe(4);
    expect(result.sessions[0].preview).toBe("Prompt after missing messages");
    expect(result.sessions[0].sessionMutationStamp).toBe(101);
  });

  it("requests a resync when an unhydrated retained transcript receives a mismatched created message id", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 1,
        messages: [
          {
            id: "message-previous",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Previous answer",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 2,
      sessionId: "session-a",
      messageId: "message-new-prompt",
      messageIndex: 1,
      messageCount: 2,
      message: {
        id: "different-payload-id",
        type: "text",
        timestamp: "10:01",
        author: "you",
        text: "Latest prompt",
      },
      preview: "Latest prompt",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("requests a resync when an unhydrated retained transcript receives an invalid created message index", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 1,
        messages: [
          {
            id: "message-previous",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Previous answer",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 2,
      sessionId: "session-a",
      messageId: "message-new-prompt",
      messageIndex: -1,
      messageCount: 2,
      message: {
        id: "message-new-prompt",
        type: "text",
        timestamp: "10:01",
        author: "you",
        text: "Latest prompt",
      },
      preview: "Latest prompt",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("requests a resync when a created message index is outside the message count", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 1,
        messages: [
          {
            id: "message-previous",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Previous answer",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 2,
      sessionId: "session-a",
      messageId: "message-new-prompt",
      messageIndex: 3,
      messageCount: 2,
      message: {
        id: "message-new-prompt",
        type: "text",
        timestamp: "10:01",
        author: "you",
        text: "Latest prompt",
      },
      preview: "Latest prompt",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("requests a resync when an unhydrated retained transcript receives an invalid created message count", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 1,
        messages: [
          {
            id: "message-previous",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Previous answer",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 2,
      sessionId: "session-a",
      messageId: "message-new-prompt",
      messageIndex: 1,
      messageCount: Number.NaN,
      message: {
        id: "message-new-prompt",
        type: "text",
        timestamp: "10:01",
        author: "you",
        text: "Latest prompt",
      },
      preview: "Latest prompt",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("requests a resync when a new created message does not advance the retained transcript count", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 1,
        messages: [
          {
            id: "message-previous",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Previous answer",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 2,
      sessionId: "session-a",
      messageId: "message-new-prompt",
      messageIndex: 1,
      messageCount: 1,
      message: {
        id: "message-new-prompt",
        type: "text",
        timestamp: "10:01",
        author: "you",
        text: "Latest prompt",
      },
      preview: "Latest prompt",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("retains a contiguous created message without marking an incomplete transcript loaded", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 5,
        messages: [
          {
            id: "message-previous",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Previous answer",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 2,
      sessionId: "session-a",
      messageId: "message-new-prompt",
      messageIndex: 1,
      messageCount: 5,
      message: {
        id: "message-new-prompt",
        type: "text",
        timestamp: "10:01",
        author: "you",
        text: "Latest prompt",
      },
      preview: "Latest prompt",
      status: "active",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }
    expect(result.sessions[0].messagesLoaded).toBe(false);
    expect(result.sessions[0].messages.map((message) => message.id)).toEqual([
      "message-previous",
      "message-new-prompt",
    ]);
    expect(result.sessions[0].messageCount).toBe(5);
  });

  it("requests a resync when a created message regresses the retained transcript count", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 2,
        messages: [
          {
            id: "message-previous",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Previous answer",
          },
          {
            id: "message-new-prompt",
            type: "text",
            timestamp: "10:01",
            author: "you",
            text: "Latest prompt",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 3,
      sessionId: "session-a",
      messageId: "message-new-prompt",
      messageIndex: 1,
      messageCount: 1,
      message: {
        id: "message-new-prompt",
        type: "text",
        timestamp: "10:01",
        author: "you",
        text: "Latest prompt",
      },
      preview: "Latest prompt",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("accepts a progressive created-message replay after a final metadata snapshot", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 3,
        sessionMutationStamp: 101,
        messages: [
          {
            id: "message-existing",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Previous answer.",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 3,
      sessionId: "session-a",
      messageId: "message-stop",
      messageIndex: 1,
      messageCount: 2,
      message: {
        id: "message-stop",
        type: "text",
        timestamp: "10:01",
        author: "assistant",
        text: "Session stopped.",
      },
      preview: "Changed files.",
      status: "idle",
      sessionMutationStamp: 101,
    };

    const firstResult = applyDeltaToSessions(sessions, delta);

    expect(firstResult.kind).toBe("applied");
    if (firstResult.kind !== "applied") {
      throw new Error("expected replayed created message to apply");
    }
    expect(firstResult.sessions[0].messagesLoaded).toBe(false);
    expect(firstResult.sessions[0].messageCount).toBe(3);
    expect(firstResult.sessions[0].sessionMutationStamp).toBe(101);
    expect(
      firstResult.sessions[0].messages.map((message) => message.id),
    ).toEqual(["message-existing", "message-stop"]);

    const secondResult = applyDeltaToSessions(firstResult.sessions, {
      ...delta,
      messageId: "message-files",
      messageIndex: 2,
      messageCount: 3,
      message: {
        id: "message-files",
        type: "text",
        timestamp: "10:01",
        author: "assistant",
        text: "Changed files.",
      },
    });

    expect(secondResult.kind).toBe("applied");
    if (secondResult.kind !== "applied") {
      throw new Error("expected second replayed created message to apply");
    }
    expect(secondResult.sessions[0].messagesLoaded).toBe(true);
    expect(secondResult.sessions[0].messageCount).toBe(3);
    expect(
      secondResult.sessions[0].messages.map((message) => message.id),
    ).toEqual(["message-existing", "message-stop", "message-files"]);
  });

  it("does not let an older created-message replay overwrite current content", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: true,
        messageCount: 1,
        preview: "Current preview",
        status: "idle",
        sessionMutationStamp: 200,
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Current answer",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 3,
      sessionId: "session-a",
      messageId: "message-1",
      messageIndex: 0,
      messageCount: 1,
      message: {
        id: "message-1",
        type: "text",
        timestamp: "09:59",
        author: "assistant",
        text: "Stale answer",
      },
      preview: "Stale preview",
      status: "active",
      sessionMutationStamp: 199,
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected stale replay to be ignored");
    }
    expect(result.sessions[0]).toEqual(sessions[0]);
  });

  it("does not duplicate a replayed prompt delta while retaining an unhydrated transcript", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 2,
        sessionMutationStamp: 100,
        messages: [
          {
            id: "message-previous",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Previous answer",
          },
          {
            id: "message-new-prompt",
            type: "text",
            timestamp: "10:01",
            author: "you",
            text: "Latest prompt",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 3,
      sessionId: "session-a",
      messageId: "message-new-prompt",
      messageIndex: 1,
      messageCount: 2,
      message: {
        id: "message-new-prompt",
        type: "text",
        timestamp: "10:01",
        author: "you",
        text: "Latest prompt",
      },
      preview: "Latest prompt",
      status: "active",
      sessionMutationStamp: 101,
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected replayed prompt delta to apply");
    }

    expect(result.sessions[0].messagesLoaded).toBe(true);
    expect(result.sessions[0].messages.map((message) => message.id)).toEqual([
      "message-previous",
      "message-new-prompt",
    ]);
    expect(
      result.sessions[0].messages.filter(
        (message) => message.id === "message-new-prompt",
      ),
    ).toHaveLength(1);
    expect(result.sessions[0].sessionMutationStamp).toBe(101);
  });

  it("inserts created messages at the provided index", () => {
    const sessions = [
      makeSession("session-a", {
        messages: [
          {
            id: "message-2",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Final answer",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 2,
      sessionId: "session-a",
      messageId: "message-1",
      messageIndex: 0,
      messageCount: 2,
      message: {
        id: "message-1",
        type: "subagentResult",
        timestamp: "10:00",
        author: "assistant",
        title: "Subagent completed",
        summary: "Hidden thinking",
      },
      preview: "",
      status: "active",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].messages.map((message) => message.id)).toEqual([
      "message-1",
      "message-2",
    ]);
  });

  it("requests a resync when an unhydrated retained transcript receives a mismatched updated message id", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 1,
        messages: [
          {
            id: "approval-1",
            type: "approval",
            timestamp: "10:00",
            author: "assistant",
            title: "Run command?",
            command: "cargo check",
            detail: "Allow this command",
            decision: "pending",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageUpdated",
      revision: 3,
      sessionId: "session-a",
      messageId: "approval-1",
      messageIndex: 0,
      messageCount: 1,
      message: {
        id: "different-approval",
        type: "approval",
        timestamp: "10:00",
        author: "assistant",
        title: "Run command?",
        command: "cargo check",
        detail: "Allow this command",
        decision: "accepted",
      },
      preview: "Approved",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("requests a resync when an unhydrated retained transcript receives an invalid updated message index", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 1,
        messages: [
          {
            id: "approval-1",
            type: "approval",
            timestamp: "10:00",
            author: "assistant",
            title: "Run command?",
            command: "cargo check",
            detail: "Allow this command",
            decision: "pending",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageUpdated",
      revision: 3,
      sessionId: "session-a",
      messageId: "approval-1",
      messageIndex: -1,
      messageCount: 1,
      message: {
        id: "approval-1",
        type: "approval",
        timestamp: "10:00",
        author: "assistant",
        title: "Run command?",
        command: "cargo check",
        detail: "Allow this command",
        decision: "accepted",
      },
      preview: "Approved",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("applies whole-message updates for resolved interaction requests", () => {
    const sessions = [
      makeSession("session-a", {
        status: "approval",
        preview: "Approval needed",
        messages: [
          {
            id: "approval-1",
            type: "approval",
            timestamp: "10:00",
            author: "assistant",
            title: "Run command?",
            command: "cargo check",
            detail: "Allow this command",
            decision: "pending",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageUpdated",
      revision: 3,
      sessionId: "session-a",
      messageId: "approval-1",
      messageIndex: 0,
      messageCount: 1,
      message: {
        id: "approval-1",
        type: "approval",
        timestamp: "10:00",
        author: "assistant",
        title: "Run command?",
        command: "cargo check",
        detail: "Allow this command",
        decision: "accepted",
      },
      preview: "Approved",
      status: "active",
      sessionMutationStamp: 202,
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].status).toBe("active");
    expect(result.sessions[0].preview).toBe("Approved");
    expect(result.sessions[0].messageCount).toBe(1);
    expect(result.sessions[0].sessionMutationStamp).toBe(202);
    expect(result.sessions[0].messages[0]).toMatchObject({
      id: "approval-1",
      type: "approval",
      decision: "accepted",
    });
  });

  it("requests a resync when a whole-message update target is missing", () => {
    const sessions = [makeSession("session-a")];
    const delta: DeltaEvent = {
      type: "messageUpdated",
      revision: 3,
      sessionId: "session-a",
      messageId: "missing-message",
      messageIndex: 0,
      messageCount: 1,
      message: {
        id: "missing-message",
        type: "text",
        timestamp: "10:00",
        author: "assistant",
        text: "Final",
      },
      preview: "Final",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("requests a resync when a whole-message update carries an invalid message count", () => {
    const sessions = [
      makeSession("session-a", {
        messageCount: 1,
        messages: [
          {
            id: "approval-1",
            type: "approval",
            timestamp: "10:00",
            author: "assistant",
            title: "Run command?",
            command: "cargo check",
            detail: "Allow this command",
            decision: "pending",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageUpdated",
      revision: 4,
      sessionId: "session-a",
      messageId: "approval-1",
      messageIndex: 0,
      messageCount: Number.NaN,
      message: {
        id: "approval-1",
        type: "approval",
        timestamp: "10:00",
        author: "assistant",
        title: "Run command?",
        command: "cargo check",
        detail: "Allow this command",
        decision: "accepted",
      },
      preview: "Approved",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("requests a resync when a whole-message update regresses the known message count", () => {
    const sessions = [
      makeSession("session-a", {
        messageCount: 2,
        messages: [
          {
            id: "approval-1",
            type: "approval",
            timestamp: "10:00",
            author: "assistant",
            title: "Run command?",
            command: "cargo check",
            detail: "Allow this command",
            decision: "pending",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageUpdated",
      revision: 4,
      sessionId: "session-a",
      messageId: "approval-1",
      messageIndex: 0,
      messageCount: 1,
      message: {
        id: "approval-1",
        type: "approval",
        timestamp: "10:00",
        author: "assistant",
        title: "Run command?",
        command: "cargo check",
        detail: "Allow this command",
        decision: "accepted",
      },
      preview: "Approved",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("applies whole-message updates when the message exists at a different index", () => {
    const sessions = [
      makeSession("session-a", {
        preview: "Pending approval",
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Earlier message",
          },
          {
            id: "approval-1",
            type: "approval",
            timestamp: "10:01",
            author: "assistant",
            title: "Run command?",
            command: "cargo check",
            detail: "Allow this command",
            decision: "pending",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageUpdated",
      revision: 4,
      sessionId: "session-a",
      messageId: "approval-1",
      messageIndex: 0,
      messageCount: 2,
      message: {
        id: "approval-1",
        type: "approval",
        timestamp: "10:01",
        author: "assistant",
        title: "Run command?",
        command: "cargo check",
        detail: "Allow this command",
        decision: "accepted",
      },
      preview: "Approved",
      status: "active",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].messages.map((message) => message.id)).toEqual([
      "message-1",
      "approval-1",
    ]);
    expect(result.sessions[0].messages[1]).toMatchObject({
      id: "approval-1",
      type: "approval",
      decision: "accepted",
    });
  });

  it("requests a resync when a whole-message update payload id mismatches the event id", () => {
    const sessions = [
      makeSession("session-a", {
        messages: [
          {
            id: "approval-1",
            type: "approval",
            timestamp: "10:00",
            author: "assistant",
            title: "Run command?",
            command: "cargo check",
            detail: "Allow this command",
            decision: "pending",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageUpdated",
      revision: 4,
      sessionId: "session-a",
      messageId: "approval-1",
      messageIndex: 0,
      messageCount: 1,
      message: {
        id: "different-message",
        type: "text",
        timestamp: "10:00",
        author: "assistant",
        text: "Wrong payload",
      },
      preview: "Wrong payload",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("requests a resync when a whole-message update carries a negative index", () => {
    const sessions = [
      makeSession("session-a", {
        messages: [
          {
            id: "approval-1",
            type: "approval",
            timestamp: "10:00",
            author: "assistant",
            title: "Run command?",
            command: "cargo check",
            detail: "Allow this command",
            decision: "pending",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageUpdated",
      revision: 4,
      sessionId: "session-a",
      messageId: "approval-1",
      messageIndex: -1,
      messageCount: 1,
      message: {
        id: "approval-1",
        type: "approval",
        timestamp: "10:00",
        author: "assistant",
        title: "Run command?",
        command: "cargo check",
        detail: "Allow this command",
        decision: "accepted",
      },
      preview: "Approved",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("requests a resync when a whole-message update carries a non-integer index", () => {
    const sessions = [
      makeSession("session-a", {
        messages: [
          {
            id: "approval-1",
            type: "approval",
            timestamp: "10:00",
            author: "assistant",
            title: "Run command?",
            command: "cargo check",
            detail: "Allow this command",
            decision: "pending",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageUpdated",
      revision: 4,
      sessionId: "session-a",
      messageId: "approval-1",
      messageIndex: 0.5,
      messageCount: 1,
      message: {
        id: "approval-1",
        type: "approval",
        timestamp: "10:00",
        author: "assistant",
        title: "Run command?",
        command: "cargo check",
        detail: "Allow this command",
        decision: "accepted",
      },
      preview: "Approved",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("requests a resync when a whole-message update carries an unsafe integer index", () => {
    const sessions = [
      makeSession("session-a", {
        messages: [
          {
            id: "approval-1",
            type: "approval",
            timestamp: "10:00",
            author: "assistant",
            title: "Run command?",
            command: "cargo check",
            detail: "Allow this command",
            decision: "pending",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageUpdated",
      revision: 4,
      sessionId: "session-a",
      messageId: "approval-1",
      messageIndex: Number.MAX_SAFE_INTEGER + 1,
      messageCount: 1,
      message: {
        id: "approval-1",
        type: "approval",
        timestamp: "10:00",
        author: "assistant",
        title: "Run command?",
        command: "cargo check",
        detail: "Allow this command",
        decision: "accepted",
      },
      preview: "Approved",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("preserves the prior session stamp when a whole-message update omits it", () => {
    const sessions = [
      makeSession("session-a", {
        sessionMutationStamp: 777,
        messages: [
          {
            id: "approval-1",
            type: "approval",
            timestamp: "10:00",
            author: "assistant",
            title: "Run command?",
            command: "cargo check",
            detail: "Allow this command",
            decision: "pending",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageUpdated",
      revision: 4,
      sessionId: "session-a",
      messageId: "approval-1",
      messageIndex: 0,
      messageCount: 1,
      message: {
        id: "approval-1",
        type: "approval",
        timestamp: "10:00",
        author: "assistant",
        title: "Run command?",
        command: "cargo check",
        detail: "Allow this command",
        decision: "accepted",
      },
      preview: "Approved",
      status: "active",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].sessionMutationStamp).toBe(777);
  });

  it.each([
    {
      label: "messageUpdated",
      makeMessage: () => ({
        id: "approval-1",
        type: "approval" as const,
        timestamp: "10:00",
        author: "assistant" as const,
        title: "Run command?",
        command: "cargo check",
        detail: "Allow this command",
        decision: "pending" as const,
      }),
      makeDelta: (): SessionDeltaEvent => ({
        type: "messageUpdated",
        revision: 4,
        sessionId: "session-a",
        messageId: "approval-1",
        messageIndex: 0,
        messageCount: 1,
        message: {
          id: "approval-1",
          type: "approval",
          timestamp: "10:00",
          author: "assistant",
          title: "Run command?",
          command: "cargo check",
          detail: "Allow this command",
          decision: "accepted",
        },
        preview: "Approved",
        status: "active",
        sessionMutationStamp: 201,
      }),
      expectedMessage: {
        id: "approval-1",
        type: "approval",
        decision: "accepted",
      },
    },
    {
      label: "textDelta",
      makeMessage: () => ({
        id: "message-1",
        type: "text" as const,
        timestamp: "10:01",
        author: "assistant" as const,
        text: "Streaming",
      }),
      makeDelta: (): SessionDeltaEvent => ({
        type: "textDelta",
        revision: 5,
        sessionId: "session-a",
        messageId: "message-1",
        messageIndex: 0,
        messageCount: 1,
        delta: " answer",
        preview: "Streaming answer",
        sessionMutationStamp: 202,
      }),
      expectedMessage: {
        id: "message-1",
        type: "text",
        text: "Streaming answer",
      },
    },
    {
      label: "textReplace",
      makeMessage: () => ({
        id: "message-1",
        type: "text" as const,
        timestamp: "10:01",
        author: "assistant" as const,
        text: "Draft answer",
      }),
      makeDelta: (): SessionDeltaEvent => ({
        type: "textReplace",
        revision: 6,
        sessionId: "session-a",
        messageId: "message-1",
        messageIndex: 0,
        messageCount: 1,
        text: "Final answer",
        preview: "Final answer",
        sessionMutationStamp: 203,
      }),
      expectedMessage: {
        id: "message-1",
        type: "text",
        text: "Final answer",
      },
    },
    {
      label: "commandUpdate",
      makeMessage: () => ({
        id: "command-1",
        type: "command" as const,
        timestamp: "10:02",
        author: "assistant" as const,
        command: "pwd",
        output: "",
        status: "running" as const,
      }),
      makeDelta: (): SessionDeltaEvent => ({
        type: "commandUpdate",
        revision: 7,
        sessionId: "session-a",
        messageId: "command-1",
        messageIndex: 0,
        messageCount: 1,
        command: "pwd",
        output: "/tmp",
        status: "success",
        preview: "/tmp",
        sessionMutationStamp: 204,
      }),
      expectedMessage: {
        id: "command-1",
        type: "command",
        output: "/tmp",
        status: "success",
      },
    },
    {
      label: "parallelAgentsUpdate",
      makeMessage: () => ({
        id: "parallel-1",
        type: "parallelAgents" as const,
        timestamp: "10:03",
        author: "assistant" as const,
        agents: [
          {
            id: "reviewer",
            title: "Reviewer",
            status: "initializing" as const,
          },
        ],
      }),
      makeDelta: (): SessionDeltaEvent => ({
        type: "parallelAgentsUpdate",
        revision: 8,
        sessionId: "session-a",
        messageId: "parallel-1",
        messageIndex: 0,
        messageCount: 1,
        agents: [
          {
            id: "reviewer",
            title: "Reviewer",
            status: "running",
            detail: "Checking diffs",
          },
        ],
        preview: "Running reviewer",
        sessionMutationStamp: 205,
      }),
      expectedMessage: {
        id: "parallel-1",
        type: "parallelAgents",
        agents: [
          {
            id: "reviewer",
            title: "Reviewer",
            status: "running",
            detail: "Checking diffs",
          },
        ],
      },
    },
  ])(
    "applies retained $label deltas while the transcript is marked unhydrated",
    ({ label, makeMessage, makeDelta, expectedMessage }) => {
      const result = applyDeltaToSessions(
        [
          makeSession("session-a", {
            messagesLoaded: false,
            messageCount: 1,
            messages: [makeMessage()],
          }),
        ],
        makeDelta(),
      );

      expect(result.kind).toBe("applied");
      if (result.kind !== "applied") {
        throw new Error(`expected ${label} to apply`);
      }
      expect(result.sessions[0].messagesLoaded).toBe(false);
      expect(result.sessions[0].messages[0]).toMatchObject(expectedMessage);
    },
  );

  it("returns appliedNeedsResync with a metadata-only patch when an unhydrated delta's target message is absent", () => {
    const sessions = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 1,
        preview: "Previous",
        messages: [
          {
            id: "message-previous",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Previous answer",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "textDelta",
      revision: 9,
      sessionId: "session-a",
      messageId: "missing-message",
      messageIndex: 1,
      messageCount: 2,
      delta: " unseen chunk",
      preview: "Updated preview",
      sessionMutationStamp: 206,
    };

    const result = applyDeltaToSessions(sessions, delta);

    // The metadata patch alone keeps the sidebar fresh, but the message body
    // is missing from the retained transcript. The reducer signals
    // `appliedNeedsResync` so the caller schedules an authoritative state
    // refetch — without it, a wedged hydration leaves the latest message
    // body invisible until the user takes another action.
    expect(result.kind).toBe("appliedNeedsResync");
    if (result.kind !== "appliedNeedsResync") {
      throw new Error("expected metadata-only delta to apply with resync nudge");
    }
    expect(result.sessions[0].messagesLoaded).toBe(false);
    expect(result.sessions[0].messages).toEqual(sessions[0].messages);
    expect(result.sessions[0].messageCount).toBe(2);
    expect(result.sessions[0].preview).toBe("Updated preview");
    expect(result.sessions[0].sessionMutationStamp).toBe(206);
  });

  it.each([
    {
      label: "messageUpdated",
      makeMessage: () => ({
        id: "approval-1",
        type: "approval" as const,
        timestamp: "10:00",
        author: "assistant" as const,
        title: "Run command?",
        command: "cargo check",
        detail: "Allow this command",
        decision: "pending" as const,
      }),
      makeDelta: (): SessionDeltaEvent => ({
        type: "messageUpdated",
        revision: 4,
        sessionId: "session-a",
        messageId: "missing-approval",
        messageIndex: 1,
        messageCount: 2,
        message: {
          id: "missing-approval",
          type: "approval",
          timestamp: "10:01",
          author: "assistant",
          title: "Run command?",
          command: "ls",
          detail: "Allow listing",
          decision: "accepted",
        },
        preview: "Updated preview",
        status: "active",
        sessionMutationStamp: 301,
      }),
    },
    {
      label: "textReplace",
      makeMessage: () => ({
        id: "message-previous",
        type: "text" as const,
        timestamp: "10:00",
        author: "assistant" as const,
        text: "Previous answer",
      }),
      makeDelta: (): SessionDeltaEvent => ({
        type: "textReplace",
        revision: 6,
        sessionId: "session-a",
        messageId: "missing-message",
        messageIndex: 1,
        messageCount: 2,
        text: "New answer",
        preview: "Updated preview",
        sessionMutationStamp: 302,
      }),
    },
    {
      label: "commandUpdate",
      makeMessage: () => ({
        id: "command-previous",
        type: "command" as const,
        timestamp: "10:02",
        author: "assistant" as const,
        command: "ls",
        output: "",
        status: "running" as const,
      }),
      makeDelta: (): SessionDeltaEvent => ({
        type: "commandUpdate",
        revision: 7,
        sessionId: "session-a",
        messageId: "missing-command",
        messageIndex: 1,
        messageCount: 2,
        command: "pwd",
        output: "/tmp",
        status: "success",
        preview: "Updated preview",
        sessionMutationStamp: 303,
      }),
    },
    {
      label: "parallelAgentsUpdate",
      makeMessage: () => ({
        id: "parallel-previous",
        type: "parallelAgents" as const,
        timestamp: "10:03",
        author: "assistant" as const,
        agents: [
          {
            id: "reviewer",
            title: "Reviewer",
            status: "initializing" as const,
          },
        ],
      }),
      makeDelta: (): SessionDeltaEvent => ({
        type: "parallelAgentsUpdate",
        revision: 8,
        sessionId: "session-a",
        messageId: "missing-parallel",
        messageIndex: 1,
        messageCount: 2,
        agents: [
          {
            id: "reviewer",
            title: "Reviewer",
            status: "running",
            detail: "Checking",
          },
        ],
        preview: "Updated preview",
        sessionMutationStamp: 304,
      }),
    },
  ])(
    "returns appliedNeedsResync for missing-target $label on an unhydrated session",
    ({ makeMessage, makeDelta }) => {
      const message = makeMessage();
      const sessions = [
        makeSession("session-a", {
          messagesLoaded: false,
          messageCount: 1,
          preview: "Previous",
          messages: [message],
        }),
      ];

      const result = applyDeltaToSessions(sessions, makeDelta());

      expect(result.kind).toBe("appliedNeedsResync");
      if (result.kind !== "appliedNeedsResync") {
        throw new Error(
          "expected unhydrated missing-target delta to apply with resync nudge",
        );
      }
      expect(result.sessions[0].messagesLoaded).toBe(false);
      expect(result.sessions[0].messages).toEqual([message]);
      expect(result.sessions[0].messageCount).toBe(2);
      expect(result.sessions[0].preview).toBe("Updated preview");
    },
  );

  it("applies text deltas to an existing message", () => {
    const sessions = [
      makeSession("session-a", {
        preview: "Hi",
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Hi",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "textDelta",
      revision: 2,
      sessionId: "session-a",
      messageId: "message-1",
      messageIndex: 0,
      messageCount: 1,
      delta: " there",
      preview: "Hi there",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].preview).toBe("Hi there");
    expect(result.sessions[0].messageCount).toBe(1);
    expect(result.sessions[0].messages[0]).toMatchObject({
      id: "message-1",
      type: "text",
      text: "Hi there",
      timestamp: "10:00",
    });
  });

  it("applies conversation marker create, update, and delete deltas", () => {
    const sessions = [
      makeSession("session-a", {
        sessionMutationStamp: 10,
        messagesLoaded: true,
        markers: [],
      }),
    ];

    const createResult = applyDeltaToSessions(sessions, {
      type: "conversationMarkerCreated",
      revision: 11,
      sessionId: "session-a",
      marker: {
        id: "marker-1",
        sessionId: "session-a",
        kind: "decision",
        name: "Decision",
        color: "#3b82f6",
        messageId: "message-1",
        messageIndexHint: 0,
        createdAt: "10:00:00",
        updatedAt: "10:00:00",
        createdBy: "user",
      },
      sessionMutationStamp: 11,
    });
    expect(createResult.kind).toBe("applied");
    if (createResult.kind !== "applied") {
      throw new Error("expected marker create to apply");
    }
    expect(createResult.sessions[0].markers).toHaveLength(1);
    expect(createResult.sessions[0].sessionMutationStamp).toBe(11);

    const updateResult = applyDeltaToSessions(createResult.sessions, {
      type: "conversationMarkerUpdated",
      revision: 12,
      sessionId: "session-a",
      marker: {
        id: "marker-1",
        sessionId: "session-a",
        kind: "checkpoint",
        name: "Checkpoint",
        color: "#22c55e",
        messageId: "message-1",
        messageIndexHint: 0,
        endMessageId: "message-2",
        endMessageIndexHint: 1,
        createdAt: "10:00:00",
        updatedAt: "10:01:00",
        createdBy: "user",
      },
      sessionMutationStamp: 12,
    });
    expect(updateResult.kind).toBe("applied");
    if (updateResult.kind !== "applied") {
      throw new Error("expected marker update to apply");
    }
    expect(updateResult.sessions[0].markers?.[0]).toMatchObject({
      kind: "checkpoint",
      name: "Checkpoint",
      endMessageId: "message-2",
    });

    const deleteResult = applyDeltaToSessions(updateResult.sessions, {
      type: "conversationMarkerDeleted",
      revision: 13,
      sessionId: "session-a",
      markerId: "marker-1",
      sessionMutationStamp: 13,
    });
    expect(deleteResult.kind).toBe("applied");
    if (deleteResult.kind !== "applied") {
      throw new Error("expected marker delete to apply");
    }
    expect(deleteResult.sessions[0].markers).toEqual([]);
    expect(deleteResult.sessions[0].sessionMutationStamp).toBe(13);
  });

  it("requests resync for marker deltas against summary or inconsistent sessions", () => {
    const marker = {
      id: "marker-1",
      sessionId: "session-a",
      kind: "decision",
      name: "Decision",
      color: "#3b82f6",
      messageId: "message-1",
      messageIndexHint: 0,
      createdAt: "10:00:00",
      updatedAt: "10:00:00",
      createdBy: "user",
    } as const;
    const markerDeltas: SessionDeltaEvent[] = [
      {
        type: "conversationMarkerCreated",
        revision: 11,
        sessionId: "session-a",
        marker,
        sessionMutationStamp: 11,
      },
      {
        type: "conversationMarkerUpdated",
        revision: 12,
        sessionId: "session-a",
        marker: {
          ...marker,
          name: "Updated decision",
          updatedAt: "10:01:00",
        },
        sessionMutationStamp: 12,
      },
      {
        type: "conversationMarkerDeleted",
        revision: 13,
        sessionId: "session-a",
        markerId: "marker-1",
        sessionMutationStamp: 13,
      },
    ];

    markerDeltas.forEach((delta) => {
      expect(
        applyDeltaToSessions(
          [makeSession("session-a", { messagesLoaded: false, markers: [] })],
          delta,
        ).kind,
      ).toBe("needsResync");
      expect(applyDeltaToSessions([], delta).kind).toBe("needsResync");
    });

    expect(
      applyDeltaToSessions(
        [makeSession("session-a", { messagesLoaded: true, markers: [] })],
        {
          type: "conversationMarkerCreated",
          revision: 14,
          sessionId: "session-a",
          marker: {
            ...marker,
            sessionId: "session-b",
          },
          sessionMutationStamp: 14,
        },
      ).kind,
    ).toBe("needsResync");
  });

  it("replaces text when a final authoritative update arrives", () => {
    const sessions = [
      makeSession("session-a", {
        preview: "Draft answer",
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Draft answer",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "textReplace",
      revision: 2,
      sessionId: "session-a",
      messageId: "message-1",
      messageIndex: 0,
      messageCount: 1,
      text: "Final answer",
      preview: "Final answer",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].preview).toBe("Final answer");
    expect(result.sessions[0].messageCount).toBe(1);
    expect(result.sessions[0].messages[0]).toMatchObject({
      id: "message-1",
      type: "text",
      text: "Final answer",
      timestamp: "10:00",
    });
  });

  it("applies text deltas when the message exists at a different index", () => {
    const sessions = [
      makeSession("session-a", {
        preview: "Hi",
        messages: [
          {
            id: "message-2",
            type: "thinking",
            timestamp: "10:00",
            author: "assistant",
            title: "Thinking",
            lines: ["Step 1"],
          },
          {
            id: "message-1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Hi",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "textDelta",
      revision: 3,
      sessionId: "session-a",
      messageId: "message-1",
      messageIndex: 0,
      messageCount: 2,
      delta: " there",
      preview: "Hi there",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].messages[1]).toMatchObject({
      id: "message-1",
      type: "text",
      text: "Hi there",
    });
  });

  it("requests a resync when a text delta target is missing", () => {
    const sessions = [makeSession("session-a")];
    const delta: DeltaEvent = {
      type: "textDelta",
      revision: 2,
      sessionId: "session-a",
      messageId: "missing-message",
      messageIndex: 0,
      messageCount: 1,
      delta: "hello",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("preserves command timestamps while updating an existing command message", () => {
    const sessions = [
      makeSession("session-a", {
        preview: "Running pwd",
        messages: [
          {
            id: "command-1",
            type: "command",
            timestamp: "10:05",
            author: "assistant",
            command: "pwd",
            output: "",
            status: "running",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "commandUpdate",
      revision: 3,
      sessionId: "session-a",
      messageId: "command-1",
      messageIndex: 0,
      messageCount: 1,
      command: "pwd",
      output: "/tmp",
      status: "success",
      preview: "/tmp",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].preview).toBe("/tmp");
    expect(result.sessions[0].messageCount).toBe(1);
    expect(result.sessions[0].messages[0]).toMatchObject({
      id: "command-1",
      type: "command",
      timestamp: "10:05",
      output: "/tmp",
      status: "success",
    });
  });

  it("applies command deltas when the command exists at a different index", () => {
    const sessions = [
      makeSession("session-a", {
        preview: "Running pwd",
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:04",
            author: "assistant",
            text: "Preparing command",
          },
          {
            id: "command-1",
            type: "command",
            timestamp: "10:05",
            author: "assistant",
            command: "pwd",
            output: "",
            status: "running",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "commandUpdate",
      revision: 4,
      sessionId: "session-a",
      messageId: "command-1",
      messageIndex: 0,
      messageCount: 2,
      command: "pwd",
      output: "/tmp",
      status: "success",
      preview: "/tmp",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].messages[1]).toMatchObject({
      id: "command-1",
      type: "command",
      timestamp: "10:05",
      output: "/tmp",
      status: "success",
    });
  });

  it("requests a resync when a command delta arrives before the command message exists", () => {
    const sessions = [makeSession("session-a")];
    const delta: DeltaEvent = {
      type: "commandUpdate",
      revision: 3,
      sessionId: "session-a",
      messageId: "command-1",
      messageIndex: 0,
      messageCount: 1,
      command: "pwd",
      output: "",
      status: "running",
      preview: "Running pwd",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("applies parallel-agent deltas when the message exists at a different index", () => {
    const sessions = [
      makeSession("session-a", {
        preview: "Running 1 agent",
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:04",
            author: "assistant",
            text: "Preparing reviewer",
          },
          {
            id: "parallel-1",
            type: "parallelAgents",
            timestamp: "10:05",
            author: "assistant",
            agents: [
              {
                id: "reviewer",
                title: "Reviewer",
                status: "initializing",
              },
            ],
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "parallelAgentsUpdate",
      revision: 5,
      sessionId: "session-a",
      messageId: "parallel-1",
      messageIndex: 0,
      messageCount: 2,
      agents: [
        {
          id: "reviewer",
          title: "Reviewer",
          status: "running",
          detail: "Checking diffs",
        },
      ],
      preview: "Running 1 agent",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].preview).toBe("Running 1 agent");
    expect(result.sessions[0].messageCount).toBe(2);
    expect(result.sessions[0].messages[1]).toMatchObject({
      id: "parallel-1",
      type: "parallelAgents",
      timestamp: "10:05",
      agents: [
        {
          id: "reviewer",
          title: "Reviewer",
          status: "running",
          detail: "Checking diffs",
        },
      ],
    });
  });

  it("requests a resync when a parallel-agent delta arrives before the message exists", () => {
    const sessions = [makeSession("session-a")];
    const delta: DeltaEvent = {
      type: "parallelAgentsUpdate",
      revision: 6,
      sessionId: "session-a",
      messageId: "parallel-1",
      messageIndex: 0,
      messageCount: 1,
      agents: [
        {
          id: "reviewer",
          title: "Reviewer",
          status: "running",
        },
      ],
      preview: "Running 1 agent",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("reorders an existing message when a replayed messageCreated arrives with the correct index", () => {
    const sessions = [
      makeSession("session-a", {
        messages: [
          {
            id: "message-2",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Final answer",
          },
          {
            id: "message-1",
            type: "subagentResult",
            timestamp: "10:00",
            author: "assistant",
            title: "Subagent completed",
            summary: "Hidden thinking",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 5,
      sessionId: "session-a",
      messageId: "message-1",
      messageIndex: 0,
      messageCount: 2,
      message: {
        id: "message-1",
        type: "subagentResult",
        timestamp: "10:00",
        author: "assistant",
        title: "Subagent completed",
        summary: "Hidden thinking",
      },
      preview: "",
      status: "active",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].messages.map((message) => message.id)).toEqual([
      "message-1",
      "message-2",
    ]);
  });

  it("moves an existing messageCreated replay forward to the supplied index", () => {
    const sessions = [
      makeSession("session-a", {
        messages: [
          {
            id: "message-1",
            type: "subagentResult",
            timestamp: "10:00",
            author: "assistant",
            title: "Subagent completed",
            summary: "Hidden thinking",
          },
          {
            id: "message-2",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Final answer",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 5,
      sessionId: "session-a",
      messageId: "message-1",
      messageIndex: 1,
      messageCount: 2,
      message: {
        id: "message-1",
        type: "subagentResult",
        timestamp: "10:00",
        author: "assistant",
        title: "Subagent completed",
        summary: "Hidden thinking",
      },
      preview: "",
      status: "active",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].messages.map((message) => message.id)).toEqual([
      "message-2",
      "message-1",
    ]);
  });

  it("requests a resync when a message create carries a negative index", () => {
    const sessions = [
      makeSession("session-a", {
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Existing",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 5,
      sessionId: "session-a",
      messageId: "message-2",
      messageIndex: -1,
      messageCount: 2,
      message: {
        id: "message-2",
        type: "text",
        timestamp: "10:01",
        author: "assistant",
        text: "Created",
      },
      preview: "Created",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("requests a resync when a message create carries a non-integer index", () => {
    const sessions = [
      makeSession("session-a", {
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Existing",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 5,
      sessionId: "session-a",
      messageId: "message-2",
      messageIndex: 0.5,
      messageCount: 2,
      message: {
        id: "message-2",
        type: "text",
        timestamp: "10:01",
        author: "assistant",
        text: "Created",
      },
      preview: "Created",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("requests a resync when a message create carries an unsafe integer index", () => {
    const sessions = [
      makeSession("session-a", {
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Existing",
          },
        ],
      }),
    ];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 5,
      sessionId: "session-a",
      messageId: "message-2",
      messageIndex: Number.MAX_SAFE_INTEGER + 1,
      messageCount: 2,
      message: {
        id: "message-2",
        type: "text",
        timestamp: "10:01",
        author: "assistant",
        text: "Created",
      },
      preview: "Created",
      status: "active",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });

  it("returns needsResync for an unsupported session delta payload", () => {
    const sessions = [makeSession("session-a")];
    const delta = {
      type: "unknownFutureDelta",
      revision: 7,
      sessionId: "session-a",
    } as unknown as Parameters<typeof applyDeltaToSessions>[1];

    expect(applyDeltaToSessions(sessions, delta)).toEqual({
      kind: "needsResync",
    });
  });
});
