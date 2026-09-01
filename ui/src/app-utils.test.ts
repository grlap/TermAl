import { describe, expect, it } from "vitest";
import {
  canNestedScrollableConsumeWheel,
  createDraftAttachment,
  isMonacoEditorEventTarget,
  MAX_PASTED_IMAGE_BYTES,
  messageChangeMarker,
} from "./app-utils";
import type { Message, ParallelAgentsMessage } from "./types";

describe("messageChangeMarker", () => {
  const baseParallelAgentsMessage: ParallelAgentsMessage = {
    id: "message-1",
    type: "parallelAgents",
    timestamp: "10:01",
    author: "assistant",
    agents: [
      {
        id: "agent-1",
        source: "tool",
        title: "Review backend",
        status: "running",
        detail: "Checking Rust changes",
      },
    ],
  };

  it("changes user-input markers when declinable flips", () => {
    // The Skip affordance must invalidate memoized rendering: false and
    // true must never share a marker.
    const baseUserInput: Message = {
      id: "message-input",
      type: "userInputRequest",
      timestamp: "10:01",
      author: "assistant",
      title: "Claude needs your input",
      detail: "Answer Claude's question to continue.",
      state: "pending",
      declinable: false,
      questions: [],
    };
    const declinable: Message = { ...baseUserInput, declinable: true };

    expect(messageChangeMarker(baseUserInput)).not.toBe(
      messageChangeMarker(declinable),
    );
    expect(messageChangeMarker(baseUserInput)).toBe(
      messageChangeMarker({ ...baseUserInput }),
    );
  });

  it("changes parallel-agent markers when only source changes", () => {
    const delegationMessage: ParallelAgentsMessage = {
      ...baseParallelAgentsMessage,
      agents: [
        { ...baseParallelAgentsMessage.agents[0]!, source: "delegation" },
      ],
    };

    expect(messageChangeMarker(baseParallelAgentsMessage)).not.toBe(
      messageChangeMarker(delegationMessage),
    );
  });

  it.each([
    [
      "id",
      {
        ...baseParallelAgentsMessage,
        agents: [{ ...baseParallelAgentsMessage.agents[0]!, id: "agent-2" }],
      },
    ],
    [
      "status",
      {
        ...baseParallelAgentsMessage,
        agents: [
          { ...baseParallelAgentsMessage.agents[0]!, status: "completed" },
        ],
      },
    ],
    [
      "detail length",
      {
        ...baseParallelAgentsMessage,
        agents: [
          {
            ...baseParallelAgentsMessage.agents[0]!,
            detail: "Checking Rust changes and frontend callbacks",
          },
        ],
      },
    ],
  ] satisfies [string, ParallelAgentsMessage][])(
    "changes parallel-agent markers when only %s changes",
    (_fieldName, changedMessage) => {
      expect(messageChangeMarker(baseParallelAgentsMessage)).not.toBe(
        messageChangeMarker(changedMessage),
      );
    },
  );

  it.each([
    [
      "user-input request",
      {
        declinable: false,
        id: "request-user-input",
        type: "userInputRequest",
        timestamp: "10:02",
        author: "assistant",
        title: "Choose",
        detail: "Pick one",
        questions: [],
        state: "pending",
      },
      {
        declinable: false,
        id: "request-user-input",
        type: "userInputRequest",
        timestamp: "10:02",
        author: "assistant",
        title: "Choose",
        detail: "Pick one",
        questions: [],
        state: "submitted",
        submittedAnswers: {},
      },
    ],
    [
      "MCP elicitation",
      {
        id: "request-mcp",
        type: "mcpElicitationRequest",
        timestamp: "10:02",
        author: "assistant",
        title: "Authorize",
        detail: "Continue?",
        request: {
          threadId: "thread-1",
          serverName: "server",
          mode: "url",
          elicitationId: "elicitation-1",
          message: "Continue?",
          url: "https://example.invalid",
        },
        state: "pending",
      },
      {
        id: "request-mcp",
        type: "mcpElicitationRequest",
        timestamp: "10:02",
        author: "assistant",
        title: "Authorize",
        detail: "Continue?",
        request: {
          threadId: "thread-1",
          serverName: "server",
          mode: "url",
          elicitationId: "elicitation-1",
          message: "Continue?",
          url: "https://example.invalid",
        },
        state: "submitted",
        submittedAction: "accept",
      },
    ],
    [
      "Codex app request",
      {
        id: "request-app",
        type: "codexAppRequest",
        timestamp: "10:02",
        author: "assistant",
        title: "Open app",
        detail: "Open connector",
        method: "open",
        params: {},
        state: "pending",
      },
      {
        id: "request-app",
        type: "codexAppRequest",
        timestamp: "10:02",
        author: "assistant",
        title: "Open app",
        detail: "Open connector",
        method: "open",
        params: {},
        state: "submitted",
        submittedResult: {},
      },
    ],
  ] satisfies [string, Message, Message][])(
    "changes markers when a %s state changes",
    (_label, pending, submitted) => {
      expect(messageChangeMarker(pending)).not.toBe(
        messageChangeMarker(submitted),
      );
    },
  );

  it.each([
    [
      "user-input answer",
      {
        declinable: false,
        id: "request-user-input",
        type: "userInputRequest",
        timestamp: "10:02",
        author: "assistant",
        title: "Choose",
        detail: "Pick one",
        questions: [],
        state: "submitted",
        submittedAnswers: { choice: ["one"] },
      },
      { submittedAnswers: { choice: ["two"] } },
    ],
    [
      "MCP submitted content",
      {
        id: "request-mcp",
        type: "mcpElicitationRequest",
        timestamp: "10:02",
        author: "assistant",
        title: "Authorize",
        detail: "Continue?",
        request: {
          threadId: "thread-1",
          serverName: "server",
          mode: "form",
          message: "Continue?",
          requestedSchema: { type: "object", properties: {} },
        },
        state: "submitted",
        submittedAction: "accept",
        submittedContent: { choice: "one" },
      },
      { submittedContent: { choice: "two" } },
    ],
    [
      "Codex app submitted result",
      {
        id: "request-app",
        type: "codexAppRequest",
        timestamp: "10:02",
        author: "assistant",
        title: "Open app",
        detail: "Open connector",
        method: "open",
        params: {},
        state: "submitted",
        submittedResult: { outcome: "one" },
      },
      { submittedResult: { outcome: "two" } },
    ],
  ] satisfies [string, Message, Partial<Message>][])(
    "changes markers when submitted %s changes without changing state or shape",
    (_label, original, update) => {
      expect(messageChangeMarker(original)).not.toBe(
        messageChangeMarker({ ...original, ...update } as Message),
      );
    },
  );

  it("returns a defensive marker for a future runtime message variant", () => {
    expect(
      messageChangeMarker({ type: "futureMessage" } as unknown as Message),
    ).toBe("unknown:futureMessage");
  });
});

describe("isMonacoEditorEventTarget", () => {
  it("detects events from Monaco editor descendants within the pane boundary", () => {
    const pane = document.createElement("section");
    const editor = document.createElement("div");
    editor.className = "monaco-editor";
    const target = document.createElement("canvas");
    editor.appendChild(target);
    pane.appendChild(editor);

    expect(isMonacoEditorEventTarget(target, pane)).toBe(true);
  });

  it("ignores Monaco-looking nodes outside the pane boundary", () => {
    const pane = document.createElement("section");
    const editor = document.createElement("div");
    editor.className = "monaco-editor";
    const target = document.createElement("canvas");
    editor.appendChild(target);

    expect(isMonacoEditorEventTarget(target, pane)).toBe(false);
  });
});

describe("canNestedScrollableConsumeWheel", () => {
  it("lets Monaco consume wheel gestures even without a native overflow scroller", () => {
    const paneScroller = document.createElement("section");
    const editor = document.createElement("div");
    editor.className = "monaco-code-editor";
    const target = document.createElement("canvas");
    editor.appendChild(target);
    paneScroller.appendChild(editor);

    expect(canNestedScrollableConsumeWheel(target, paneScroller, 120)).toBe(true);
  });
});

describe("createDraftAttachment", () => {
  it("accepts an image at the 10 MiB boundary", async () => {
    const file = new File(
      [new Uint8Array(MAX_PASTED_IMAGE_BYTES)],
      "pasted.png",
      { type: "image/png" },
    );

    const attachment = await createDraftAttachment(file, 0);

    expect(attachment.byteSize).toBe(10 * 1024 * 1024);
    expect(attachment.fileName).toBe("pasted.png");
    expect(attachment.mediaType).toBe("image/png");
  });

  it("rejects an image one byte over 10 MiB", async () => {
    const file = new File(
      [new Uint8Array(MAX_PASTED_IMAGE_BYTES + 1)],
      "too-large.png",
      { type: "image/png" },
    );

    await expect(createDraftAttachment(file, 0)).rejects.toThrow(
      "Pasted image exceeds the 10 MB limit.",
    );
  });
});
