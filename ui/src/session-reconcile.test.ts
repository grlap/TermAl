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
});
