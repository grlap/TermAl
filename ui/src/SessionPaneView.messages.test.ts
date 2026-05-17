import { describe, expect, it } from "vitest";

import {
  commandMessagesForPaneViewMode,
  diffMessagesForPaneViewMode,
  latestAssistantMessageIdForSession,
  paneViewModeDefaultsToBottomScroll,
  visibleMessagesForPaneViewMode,
} from "./SessionPaneView.messages";
import type { CommandMessage, DiffMessage, Message, Session } from "./types";
import type { PaneViewMode } from "./workspace";

function textMessage(id: string, author: Message["author"]): Message {
  return {
    id,
    type: "text",
    author,
    text: id,
    timestamp: "now",
  };
}

function commandMessage(id: string): CommandMessage {
  return {
    id,
    type: "command",
    author: "assistant",
    timestamp: "now",
    command: "echo ok",
    output: "ok",
    status: "success",
  };
}

function diffMessage(id: string): DiffMessage {
  return {
    id,
    type: "diff",
    author: "assistant",
    timestamp: "now",
    filePath: "src/main.rs",
    summary: "Changed src/main.rs",
    diff: "@@",
    changeType: "edit",
  };
}

function session(messages: Message[]): Session {
  return {
    id: "session-1",
    name: "Session",
    emoji: "",
    agent: "Codex",
    workdir: "/tmp/project",
    model: "default",
    status: "idle",
    preview: "",
    messages,
    pendingPrompts: [],
  };
}

describe("SessionPaneView message helpers", () => {
  it("filters command and diff messages only in their matching modes", () => {
    const command = commandMessage("command-1");
    const diff = diffMessage("diff-1");
    const user = textMessage("user-1", "you");
    const activeSession = session([user, command, diff]);

    expect(commandMessagesForPaneViewMode("commands", activeSession)).toEqual([
      command,
    ]);
    expect(commandMessagesForPaneViewMode("session", activeSession)).toEqual([]);
    expect(diffMessagesForPaneViewMode("diffs", activeSession)).toEqual([diff]);
    expect(diffMessagesForPaneViewMode("commands", activeSession)).toEqual([]);
  });

  it("selects visible messages for command and diff modes only", () => {
    const command = commandMessage("command-1");
    const diff = diffMessage("diff-1");

    expect(visibleMessagesForPaneViewMode("commands", [command], [diff])).toEqual([
      command,
    ]);
    expect(visibleMessagesForPaneViewMode("diffs", [command], [diff])).toEqual([
      diff,
    ]);
    expect(visibleMessagesForPaneViewMode("session", [command], [diff])).toEqual(
      [],
    );
  });

  it("keeps the same default bottom-scroll modes", () => {
    const bottomModes: PaneViewMode[] = ["session", "commands", "diffs"];
    const nonBottomModes: PaneViewMode[] = [
      "prompt",
      "source",
      "gitStatus",
      "filesystem",
      "canvas",
      "terminal",
      "instructionDebugger",
    ];

    expect(bottomModes.every(paneViewModeDefaultsToBottomScroll)).toBe(true);
    expect(nonBottomModes.some(paneViewModeDefaultsToBottomScroll)).toBe(false);
  });

  it("returns the newest assistant message id", () => {
    expect(
      latestAssistantMessageIdForSession(
        session([
          textMessage("assistant-1", "assistant"),
          textMessage("user-1", "you"),
          textMessage("assistant-2", "assistant"),
        ]),
      ),
    ).toBe("assistant-2");
    expect(latestAssistantMessageIdForSession(session([]))).toBeNull();
    expect(latestAssistantMessageIdForSession(null)).toBeNull();
  });
});
