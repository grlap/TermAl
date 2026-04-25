import {
  LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS,
  applyDeltaToSessions,
  pruneLiveTransportActivitySessions,
  sessionHasPotentiallyStaleTransport,
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

  it("does not treat a newer user-authored turn as having current-turn assistant activity", () => {
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
    ).toBe(false);
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

  it("updates metadata for unhydrated summaries without forcing a state resync", () => {
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
      messageIndex: 0,
      messageCount: 1,
      message: {
        id: "different-payload-id",
        type: "text",
        timestamp: "10:00",
        author: "assistant",
        text: "Dropped until hydration",
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
    expect(result.sessions[0].messages).toEqual([]);
    expect(result.sessions[0].status).toBe("active");
    expect(result.sessions[0].preview).toBe("Waiting for activity.");
    expect(result.sessions[0].messageCount).toBe(1);
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
