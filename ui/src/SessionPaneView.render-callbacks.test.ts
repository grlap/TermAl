import { describe, expect, it } from "vitest";
import {
  shouldPreferStreamingAssistantTextRender,
  streamingAssistantTextMessageIdForSession,
} from "./SessionPaneView.render-callbacks";
import type { Message, Session } from "./types";

function makeSession(
  status: Session["status"],
  messages: Message[],
): Session {
  return {
    id: "session-1",
    name: "Session",
    emoji: "",
    agent: "Codex",
    workdir: "/repo",
    model: "gpt-5.5",
    status,
    preview: "",
    messages,
  };
}

const assistantTable: Message = {
  id: "assistant-table",
  type: "text",
  author: "assistant",
  timestamp: "10:01",
  text: [
    "Tracked Project Total",
    "",
    "| Group | Files | Lines | Size |",
    "| --- | ---: | ---: | ---: |",
    "| Backend |",
  ].join("\n"),
};

const approvalCard: Message = {
  id: "approval-1",
  type: "approval",
  author: "assistant",
  timestamp: "10:02",
  title: "Codex wants approval",
  command: "git status",
  detail: "Inspect the repository.",
  decision: "pending",
};

describe("SessionPaneView render callbacks", () => {
  it("streams only the active assistant text that is the last transcript item", () => {
    const session = makeSession("active", [assistantTable]);
    const streamingTextId = streamingAssistantTextMessageIdForSession(session);

    expect(streamingTextId).toBe("assistant-table");
    expect(
      shouldPreferStreamingAssistantTextRender(
        assistantTable,
        streamingTextId,
      ),
    ).toBe(true);
  });

  it("does not put prior assistant text back on the streaming path after a prompt or approval card follows it", () => {
    const userPrompt: Message = {
      id: "user-2",
      type: "text",
      author: "you",
      timestamp: "10:03",
      text: "Continue.",
    };

    for (const session of [
      makeSession("active", [assistantTable, userPrompt]),
      makeSession("approval", [assistantTable, approvalCard]),
      makeSession("idle", [assistantTable]),
    ]) {
      const streamingTextId = streamingAssistantTextMessageIdForSession(session);

      expect(streamingTextId).toBeNull();
      expect(
        shouldPreferStreamingAssistantTextRender(
          assistantTable,
          streamingTextId,
        ),
      ).toBe(false);
    }
  });
});
