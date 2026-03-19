import { applyDeltaToSessions } from "./live-updates";
import type { DeltaEvent, Session } from "./types";

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
});
