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
      sessionId: "session-a",
      messageId: "message-1",
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

  it("requests a resync when a text delta target is missing", () => {
    const sessions = [makeSession("session-a")];
    const delta: DeltaEvent = {
      type: "textDelta",
      sessionId: "session-a",
      messageId: "missing-message",
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
      sessionId: "session-a",
      messageId: "command-1",
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

  it("requests a resync when a command delta arrives before the command message exists", () => {
    const sessions = [makeSession("session-a")];
    const delta: DeltaEvent = {
      type: "commandUpdate",
      sessionId: "session-a",
      messageId: "command-1",
      command: "pwd",
      output: "",
      status: "running",
      preview: "Running pwd",
    };

    expect(applyDeltaToSessions(sessions, delta)).toEqual({ kind: "needsResync" });
  });
});
