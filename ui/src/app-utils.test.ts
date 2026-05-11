import { describe, expect, it } from "vitest";
import {
  canNestedScrollableConsumeWheel,
  isMonacoEditorEventTarget,
  messageChangeMarker,
} from "./app-utils";
import type { ParallelAgentsMessage } from "./types";

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
