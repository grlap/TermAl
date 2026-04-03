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
  pending: UserInputRequestMessage | McpElicitationRequestMessage | CodexAppRequestMessage;
  resolved: UserInputRequestMessage | McpElicitationRequestMessage | CodexAppRequestMessage;
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

    expect(Array.from(liveTransportActivityAtBySessionId.entries())).toEqual([]);
  });

  it("does nothing when transport activity is already empty", () => {
    const liveTransportActivityAtBySessionId = new Map<string, number>();

    pruneLiveTransportActivitySessions(liveTransportActivityAtBySessionId, [
      makeSession("session-1"),
    ]);

    expect(Array.from(liveTransportActivityAtBySessionId.entries())).toEqual([]);
  });

});

describe("applyDeltaToSessions", () => {
  it("appends a created message without needing a resync", () => {
    const sessions = [makeSession("session-a")];
    const delta: DeltaEvent = {
      type: "messageCreated",
      revision: 1,
      sessionId: "session-a",
      messageId: "message-1",
      messageIndex: 0,
      message: {
        id: "message-1",
        type: "text",
        timestamp: "10:00",
        author: "assistant",
        text: "",
      },
      preview: "Waiting for activity.",
      status: "active",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].status).toBe("active");
    expect(result.sessions[0].messages).toHaveLength(1);
    expect(result.sessions[0].messages[0]).toMatchObject({
      id: "message-1",
      type: "text",
      text: "",
    });
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

    expect(result.sessions[0].messages.map((message) => message.id)).toEqual(["message-1", "message-2"]);
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
      delta: " there",
      preview: "Hi there",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].preview).toBe("Hi there");
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
      text: "Final answer",
      preview: "Final answer",
    };

    const result = applyDeltaToSessions(sessions, delta);

    expect(result.kind).toBe("applied");
    if (result.kind !== "applied") {
      throw new Error("expected delta to apply");
    }

    expect(result.sessions[0].preview).toBe("Final answer");
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
      delta: "hello",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({ kind: "needsResync" });
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
      command: "pwd",
      output: "",
      status: "running",
      preview: "Running pwd",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({ kind: "needsResync" });
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
      agents: [
        {
          id: "reviewer",
          title: "Reviewer",
          status: "running",
        },
      ],
      preview: "Running 1 agent",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({ kind: "needsResync" });
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

    expect(result.sessions[0].messages.map((message) => message.id)).toEqual(["message-1", "message-2"]);
  });
  it("returns needsResync for an unsupported session delta payload", () => {
    const sessions = [makeSession("session-a")];
    const delta = {
      type: "unknownFutureDelta",
      revision: 7,
      sessionId: "session-a",
    } as unknown as Parameters<typeof applyDeltaToSessions>[1];

    expect(applyDeltaToSessions(sessions, delta)).toEqual({ kind: "needsResync" });
  });
});
