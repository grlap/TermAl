import type { Session } from "./types";
import { reconcileSessions } from "./session-reconcile";

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

function expectChangedMessageReference(
  previousMessage: Session["messages"][number],
  nextMessage: Session["messages"][number],
) {
  const previous = [
    makeSession("session-a", {
      messages: [
        {
          id: "message-stable",
          type: "text",
          timestamp: "10:00",
          author: "assistant",
          text: "Stable",
        },
        previousMessage,
      ],
    }),
  ];

  const next = [
    makeSession("session-a", {
      messages: [
        {
          id: "message-stable",
          type: "text",
          timestamp: "10:00",
          author: "assistant",
          text: "Stable",
        },
        nextMessage,
      ],
    }),
  ];

  const merged = reconcileSessions(previous, next);

  expect(merged).not.toBe(previous);
  expect(merged[0]).not.toBe(previous[0]);
  expect(merged[0].messages).not.toBe(previous[0].messages);
  expect(merged[0].messages[0]).toBe(previous[0].messages[0]);
  expect(merged[0].messages[1]).not.toBe(previous[0].messages[1]);
  expect(merged[0].messages[1]).toBe(next[0].messages[1]);
  expect(merged[0].messages[1]).toEqual(nextMessage);
}

function expectStableMessageReference(message: Session["messages"][number]) {
  const previous = [
    makeSession("session-a", {
      messages: [
        {
          id: "message-stable",
          type: "text",
          timestamp: "10:00",
          author: "assistant",
          text: "Stable",
        },
        message,
      ],
    }),
  ];

  const next = [
    makeSession("session-a", {
      messages: [
        structuredClone(previous[0].messages[0]),
        structuredClone(message),
      ],
    }),
  ];

  const merged = reconcileSessions(previous, next);

  expect(merged).toBe(previous);
  expect(merged[0]).toBe(previous[0]);
  expect(merged[0].messages).toBe(previous[0].messages);
  expect(merged[0].messages[0]).toBe(previous[0].messages[0]);
  expect(merged[0].messages[1]).toBe(previous[0].messages[1]);
  expect(merged[0].messages[1]).toEqual(message);
}

describe("reconcileSessions", () => {
  it("reuses the existing session object when nothing changed", () => {
    const previous = [
      makeSession("session-a", {
        preview: "ready",
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Hello",
          },
        ],
      }),
    ];

    const next = [
      makeSession("session-a", {
        preview: "ready",
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Hello",
          },
        ],
      }),
    ];

    const merged = reconcileSessions(previous, next);

    expect(merged).toBe(previous);
    expect(merged[0]).toBe(previous[0]);
    expect(merged[0].messages).toBe(previous[0].messages);
    expect(merged[0].messages[0]).toBe(previous[0].messages[0]);
  });

  it("adopts marker changes from summary session snapshots", () => {
    const previous = [
      makeSession("session-a", {
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Hello",
          },
        ],
        markers: [],
      }),
    ];
    const next = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 1,
        markers: [
          {
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
        ],
      }),
    ];

    const merged = reconcileSessions(previous, next);

    expect(merged).not.toBe(previous);
    expect(merged[0].messages).toBe(previous[0].messages);
    expect(merged[0].markers).toEqual(next[0].markers);
  });

  it("reuses the existing session object when the mutation stamp matches", () => {
    const previous = [
      makeSession("session-a", {
        preview: "ready",
        sessionMutationStamp: 42,
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Hello",
          },
        ],
      }),
    ];

    const next = [
      makeSession("session-a", {
        preview: "ready",
        sessionMutationStamp: 42,
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Hello",
          },
        ],
      }),
    ];

    const merged = reconcileSessions(previous, next);

    expect(merged).toBe(previous);
    expect(merged[0]).toBe(previous[0]);
  });

  it("does not let the mutation-stamp fast path block unloaded-to-loaded promotion", () => {
    const previous = [
      makeSession("session-a", {
        preview: "ready",
        messagesLoaded: false,
        messageCount: 1,
        sessionMutationStamp: 42,
        messages: [],
      }),
    ];

    const next = [
      makeSession("session-a", {
        preview: "ready",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 42,
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

    const merged = reconcileSessions(previous, next);

    expect(merged).not.toBe(previous);
    expect(merged[0]).not.toBe(previous[0]);
    expect(merged[0].messagesLoaded).toBe(true);
    expect(merged[0].messages).toHaveLength(1);
    expect(merged[0].messages[0]).toMatchObject({
      id: "message-1",
      text: "Hydrated transcript",
    });
  });

  it("reuses an explicitly loaded session when the mutation stamp still matches", () => {
    const previous = [
      makeSession("session-a", {
        preview: "ready",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 42,
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Loaded transcript",
          },
        ],
      }),
    ];

    const next = [
      makeSession("session-a", {
        preview: "ready",
        messagesLoaded: true,
        messageCount: 1,
        sessionMutationStamp: 42,
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Loaded transcript",
          },
        ],
      }),
    ];

    const merged = reconcileSessions(previous, next);

    expect(merged).toBe(previous);
    expect(merged[0]).toBe(previous[0]);
    expect(merged[0].messages).toBe(previous[0].messages);
  });

  it("can disable the mutation-stamp fast path after a server restart", () => {
    const previous = [
      makeSession("session-a", {
        preview: "ready",
        sessionMutationStamp: 42,
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Old server text",
          },
        ],
      }),
    ];

    const next = [
      makeSession("session-a", {
        preview: "ready",
        sessionMutationStamp: 42,
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Restarted server text",
          },
        ],
      }),
    ];

    const merged = reconcileSessions(previous, next, {
      disableMutationStampFastPath: true,
    });

    expect(merged).not.toBe(previous);
    expect(merged[0]).not.toBe(previous[0]);
    expect(merged[0].messages[0]).toMatchObject({
      id: "message-1",
      text: "Restarted server text",
    });
  });

  it("marks hydrated messages stale when a metadata-only summary reports a higher message count", () => {
    const previous = [
      makeSession("session-a", {
        messagesLoaded: true,
        preview: "Old preview",
        messageCount: 1,
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Loaded transcript",
          },
        ],
      }),
    ];

    const next = [
      makeSession("session-a", {
        messagesLoaded: false,
        preview: "New preview",
        status: "active",
        messageCount: 2,
        messages: [],
      }),
    ];

    const merged = reconcileSessions(previous, next);

    expect(merged).not.toBe(previous);
    expect(merged[0].messagesLoaded).toBe(false);
    expect(merged[0].preview).toBe("New preview");
    expect(merged[0].status).toBe("active");
    expect(merged[0].messageCount).toBe(2);
    expect(merged[0].messages).toBe(previous[0].messages);
  });

  it("preserves hydrated messages when a metadata-only session summary has no count gap", () => {
    const previous = [
      makeSession("session-a", {
        messagesLoaded: true,
        preview: "Old preview",
        messageCount: 1,
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Loaded transcript",
          },
        ],
      }),
    ];

    const next = [
      makeSession("session-a", {
        messagesLoaded: false,
        preview: "New preview",
        status: "active",
        messageCount: 1,
        messages: [],
      }),
    ];

    const merged = reconcileSessions(previous, next);

    expect(merged).not.toBe(previous);
    expect(merged[0].messagesLoaded).toBe(true);
    expect(merged[0].preview).toBe("New preview");
    expect(merged[0].status).toBe("active");
    expect(merged[0].messageCount).toBe(1);
    expect(merged[0].messages).toBe(previous[0].messages);
  });

  it("marks hydrated messages stale when a metadata-only summary reports a newer mutation stamp", () => {
    const previous = [
      makeSession("session-a", {
        messagesLoaded: true,
        preview: "Old preview",
        messageCount: 1,
        sessionMutationStamp: 10,
        messages: [
          {
            id: "message-1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Loaded transcript before delayed delta.",
          },
        ],
      }),
    ];

    const next = [
      makeSession("session-a", {
        messagesLoaded: false,
        preview: "Preview from delayed delta.",
        status: "active",
        messageCount: 1,
        sessionMutationStamp: 11,
        messages: [],
      }),
    ];

    const merged = reconcileSessions(previous, next);

    expect(merged).not.toBe(previous);
    expect(merged[0].messagesLoaded).toBe(false);
    expect(merged[0].preview).toBe("Preview from delayed delta.");
    expect(merged[0].messageCount).toBe(1);
    expect(merged[0].sessionMutationStamp).toBe(11);
    expect(merged[0].messages).toBe(previous[0].messages);
  });

  it("keeps new metadata-only session summaries unhydrated", () => {
    const next = [
      makeSession("session-a", {
        messagesLoaded: false,
        messageCount: 3,
        messages: [],
      }),
    ];

    const merged = reconcileSessions([], next);

    expect(merged[0].messagesLoaded).toBe(false);
    expect(merged[0].messageCount).toBe(3);
    expect(merged[0].messages).toEqual([]);
  });

  it("forces messagesLoaded=false on summary sessions when forceMessagesUnloaded is set", () => {
    // Backend-restart scenario: persisted sessions intentionally clear
    // `sessionMutationStamp` on save/load, so the post-restart summary
    // arrives with `sessionMutationStamp: undefined`. The local transcript
    // may be a stale streaming partial whose message count happens to
    // match the server's authoritative count (e.g., the assistant chunk
    // streamed locally but the final completion was persisted then lost
    // from the SSE channel during shutdown). Without an explicit signal,
    // the reconcile would keep `messagesLoaded: true` against the local
    // count and the visible-session hydration effect would not re-fetch,
    // leaving the user staring at stale content until hard refresh.
    const previous = [
      makeSession("session-a", {
        messagesLoaded: true,
        preview: "Streaming partial",
        messageCount: 2,
        sessionMutationStamp: 42,
        messages: [
          {
            id: "message-user",
            type: "text",
            timestamp: "10:00",
            author: "you",
            text: "Hello",
          },
          {
            id: "message-assistant",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "Hello, I'll he", // partial streaming chunk
          },
        ],
      }),
    ];

    const next = [
      makeSession("session-a", {
        messagesLoaded: false,
        preview: "Streaming partial",
        messageCount: 2,
        // Persisted session loaded from SQLite has no sessionMutationStamp.
        sessionMutationStamp: undefined,
        messages: [],
      }),
    ];

    const merged = reconcileSessions(previous, next, {
      forceMessagesUnloaded: true,
    });

    expect(merged).not.toBe(previous);
    expect(merged[0].messagesLoaded).toBe(false);
    // Retain previous messages so the pane has something to show until
    // hydration completes; only the flag flips so the hydration effect
    // re-fires.
    expect(merged[0].messages).toBe(previous[0].messages);
  });

  it("reuses unchanged messages when only the streaming tail message changed", () => {
    const previous = [
      makeSession("session-a", {
        status: "active",
        preview: "Hi",
        messages: [
          {
            id: "message-1",
            type: "command",
            timestamp: "10:00",
            author: "assistant",
            command: "pwd",
            output: "/tmp",
            status: "success",
          },
          {
            id: "message-2",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Hello",
          },
        ],
      }),
    ];

    const next = [
      makeSession("session-a", {
        status: "active",
        preview: "Hello there",
        messages: [
          {
            id: "message-1",
            type: "command",
            timestamp: "10:00",
            author: "assistant",
            command: "pwd",
            output: "/tmp",
            status: "success",
          },
          {
            id: "message-2",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Hello there",
          },
        ],
      }),
    ];

    const merged = reconcileSessions(previous, next);

    expect(merged).not.toBe(previous);
    expect(merged[0]).not.toBe(previous[0]);
    expect(merged[0].messages).not.toBe(previous[0].messages);
    expect(merged[0].messages[0]).toBe(previous[0].messages[0]);
    expect(merged[0].messages[1]).not.toBe(previous[0].messages[1]);
    expect(merged[0].messages[1]).toBe(next[0].messages[1]);
    expect(merged[0].messages[1]).toEqual(next[0].messages[1]);
  });

  it("reuses unaffected sessions while replacing the changed one", () => {
    const previous = [
      makeSession("session-a", {
        preview: "stable",
        messages: [
          {
            id: "message-a1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "A",
          },
        ],
      }),
      makeSession("session-b", {
        status: "active",
        preview: "B",
        messages: [
          {
            id: "message-b1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "B",
          },
        ],
      }),
    ];

    const next = [
      makeSession("session-a", {
        preview: "stable",
        messages: [
          {
            id: "message-a1",
            type: "text",
            timestamp: "10:00",
            author: "assistant",
            text: "A",
          },
        ],
      }),
      makeSession("session-b", {
        status: "active",
        preview: "Better B",
        messages: [
          {
            id: "message-b1",
            type: "text",
            timestamp: "10:01",
            author: "assistant",
            text: "Better B",
          },
        ],
      }),
    ];

    const merged = reconcileSessions(previous, next);

    expect(merged[0]).toBe(previous[0]);
    expect(merged[1]).not.toBe(previous[1]);
  });

  it("replaces messages when language metadata changes", () => {
    const previous = [
      makeSession("session-a", {
        messages: [
          {
            id: "message-1",
            type: "command",
            timestamp: "10:00",
            author: "assistant",
            command: "cat ui/src/App.tsx",
            output: "const value = 1;",
            status: "success",
          },
        ],
      }),
    ];

    const next = [
      makeSession("session-a", {
        messages: [
          {
            id: "message-1",
            type: "command",
            timestamp: "10:00",
            author: "assistant",
            command: "cat ui/src/App.tsx",
            commandLanguage: "bash",
            output: "const value = 1;",
            outputLanguage: "typescript",
            status: "success",
          },
        ],
      }),
    ];

    const merged = reconcileSessions(previous, next);

    expect(merged).not.toBe(previous);
    expect(merged[0]).not.toBe(previous[0]);
    expect(merged[0].messages).not.toBe(previous[0].messages);
    expect(merged[0].messages[0]).not.toBe(previous[0].messages[0]);
  });

  it("replaces user input request messages when their state changes", () => {
    expectChangedMessageReference(
      {
        id: "message-1",
        type: "userInputRequest",
        timestamp: "10:01",
        author: "assistant",
        title: "Need input",
        detail: "Choose one option",
        state: "pending",
        questions: [
          {
            header: "Mode",
            id: "mode",
            question: "Choose a mode",
            options: [
              {
                label: "Fast",
                description: "Uses the fast mode",
              },
            ],
          },
        ],
      },
      {
        id: "message-1",
        type: "userInputRequest",
        timestamp: "10:01",
        author: "assistant",
        title: "Need input",
        detail: "Choose one option",
        state: "submitted",
        questions: [
          {
            header: "Mode",
            id: "mode",
            question: "Choose a mode",
            options: [
              {
                label: "Fast",
                description: "Uses the fast mode",
              },
            ],
          },
        ],
        submittedAnswers: {
          mode: ["Fast"],
        },
      },
    );
  });

  it("reuses user input request messages when nothing changed", () => {
    expectStableMessageReference({
      id: "message-1",
      type: "userInputRequest",
      timestamp: "10:01",
      author: "assistant",
      title: "Need input",
      detail: "Choose one option",
      state: "pending",
      questions: [
        {
          header: "Mode",
          id: "mode",
          question: "Choose a mode",
          options: [
            {
              label: "Fast",
              description: "Uses the fast mode",
            },
          ],
        },
      ],
    });
  });

  it("replaces MCP elicitation request messages when their state changes", () => {
    expectChangedMessageReference(
      {
        id: "message-1",
        type: "mcpElicitationRequest",
        timestamp: "10:01",
        author: "assistant",
        title: "Need MCP input",
        detail: "Confirm the action",
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
      {
        id: "message-1",
        type: "mcpElicitationRequest",
        timestamp: "10:01",
        author: "assistant",
        title: "Need MCP input",
        detail: "Confirm the action",
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
    );
  });

  it("reuses MCP elicitation request messages when nothing changed", () => {
    expectStableMessageReference({
      id: "message-1",
      type: "mcpElicitationRequest",
      timestamp: "10:01",
      author: "assistant",
      title: "Need MCP input",
      detail: "Confirm the action",
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
    });
  });

  it("replaces Codex app request messages when their state changes", () => {
    expectChangedMessageReference(
      {
        id: "message-1",
        type: "codexAppRequest",
        timestamp: "10:01",
        author: "assistant",
        title: "App request",
        detail: "Return some JSON",
        method: "workspace.pick_file",
        params: {
          allowMultiple: false,
        },
        state: "pending",
      },
      {
        id: "message-1",
        type: "codexAppRequest",
        timestamp: "10:01",
        author: "assistant",
        title: "App request",
        detail: "Return some JSON",
        method: "workspace.pick_file",
        params: {
          allowMultiple: false,
        },
        state: "submitted",
        submittedResult: {
          path: "/repo/src/main.ts",
        },
      },
    );
  });

  it("reuses Codex app request messages when nothing changed", () => {
    expectStableMessageReference({
      id: "message-1",
      type: "codexAppRequest",
      timestamp: "10:01",
      author: "assistant",
      title: "App request",
      detail: "Return some JSON",
      method: "workspace.pick_file",
      params: {
        allowMultiple: false,
      },
      state: "pending",
    });
  });

  it("replaces a session when the external session id changes", () => {
    const previous = [makeSession("session-a", { externalSessionId: null, preview: "ready" })];

    const next = [
      makeSession("session-a", {
        externalSessionId: "019cd7b9-551b-7200-9af4-afa006a74db7",
        preview: "ready",
      }),
    ];

    const merged = reconcileSessions(previous, next);

    expect(merged).not.toBe(previous);
    expect(merged[0]).not.toBe(previous[0]);
    expect(merged[0].externalSessionId).toBe("019cd7b9-551b-7200-9af4-afa006a74db7");
  });

  it("replaces a session when the Codex thread state changes", () => {
    const previous = [
      makeSession("session-a", {
        externalSessionId: "thread-1",
        codexThreadState: "active",
        preview: "ready",
      }),
    ];

    const next = [
      makeSession("session-a", {
        externalSessionId: "thread-1",
        codexThreadState: "archived",
        preview: "ready",
      }),
    ];

    const merged = reconcileSessions(previous, next);

    expect(merged).not.toBe(previous);
    expect(merged[0]).not.toBe(previous[0]);
    expect(merged[0].codexThreadState).toBe("archived");
  });
  it("replaces a session when the project id changes", () => {
    const previous = [makeSession("session-a", { projectId: "project-a", preview: "ready" })];

    const next = [
      makeSession("session-a", {
        projectId: "project-b",
        preview: "ready",
      }),
    ];

    const merged = reconcileSessions(previous, next);

    expect(merged).not.toBe(previous);
    expect(merged[0]).not.toBe(previous[0]);
    expect(merged[0].projectId).toBe("project-b");
  });

});

