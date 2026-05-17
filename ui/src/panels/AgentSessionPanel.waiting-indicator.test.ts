import { describe, expect, it } from "vitest";
import { shouldShowAgentSessionWaitingIndicator } from "./AgentSessionPanel.waiting-indicator";
import type { Message } from "../types";

const userMessage: Message = {
  id: "message-user",
  type: "text",
  timestamp: "10:00",
  author: "you",
  text: "Prompt",
};

const assistantMessage: Message = {
  id: "message-assistant",
  type: "text",
  timestamp: "10:01",
  author: "assistant",
  text: "Answer",
};

const fileChangesMessage: Message = {
  id: "message-files",
  type: "fileChanges",
  timestamp: "10:02",
  author: "assistant",
  title: "Agent changed 1 file",
  files: [{ path: "src/main.rs", kind: "modified" }],
};

describe("shouldShowAgentSessionWaitingIndicator", () => {
  it("hides when the upstream waiting flag is false", () => {
    expect(
      shouldShowAgentSessionWaitingIndicator({
        showWaitingIndicator: false,
        waitingIndicatorKind: "liveTurn",
        sessionStatus: "active",
        visibleMessages: [userMessage],
      }),
    ).toBe(false);
  });

  it("always keeps send and delegation waits visible", () => {
    for (const waitingIndicatorKind of ["send", "delegationWait"] as const) {
      expect(
        shouldShowAgentSessionWaitingIndicator({
          showWaitingIndicator: true,
          waitingIndicatorKind,
          sessionStatus: "idle",
          visibleMessages: [userMessage, fileChangesMessage],
        }),
      ).toBe(true);
    }
  });

  it("suppresses stale live-turn waits after assistant output", () => {
    expect(
      shouldShowAgentSessionWaitingIndicator({
        showWaitingIndicator: true,
        waitingIndicatorKind: "liveTurn",
        sessionStatus: "idle",
        visibleMessages: [userMessage, assistantMessage],
      }),
    ).toBe(false);
  });

  it("keeps active live-turn waits visible until turn-finalizing output appears", () => {
    expect(
      shouldShowAgentSessionWaitingIndicator({
        showWaitingIndicator: true,
        waitingIndicatorKind: "liveTurn",
        sessionStatus: "active",
        visibleMessages: [userMessage, assistantMessage],
      }),
    ).toBe(true);

    expect(
      shouldShowAgentSessionWaitingIndicator({
        showWaitingIndicator: true,
        waitingIndicatorKind: "liveTurn",
        sessionStatus: "active",
        visibleMessages: [userMessage, fileChangesMessage],
      }),
    ).toBe(false);
  });
});
