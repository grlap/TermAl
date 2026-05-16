import { describe, expect, it, vi } from "vitest";
import { ApiRequestError } from "../api";
import type { AgentCommand } from "../types";
import {
  formatAgentCommandResolverError,
  prepareAgentCommandSubmission,
  sendResolvedAgentCommandSubmission,
  splitAgentCommandResolverTail,
} from "./session-agent-command-submission";
import type { SlashPaletteItem } from "./session-slash-palette";

function agentCommandItem(
  overrides: Partial<Extract<SlashPaletteItem, { kind: "agent-command" }>> = {},
): Extract<SlashPaletteItem, { kind: "agent-command" }> {
  const command: AgentCommand = {
    content: "Create a follow-up task",
    description: "Create a follow-up task",
    kind: "promptTemplate",
    name: "task",
    source: ".termal/commands/task.md",
  };
  return {
    command,
    detail: "Create a follow-up task",
    hasArguments: true,
    key: "agent-command:task",
    kind: "agent-command",
    label: "/task",
    name: "task",
    ...overrides,
  };
}

describe("session agent command submission helpers", () => {
  it("splits resolver arguments from optional note text", () => {
    expect(splitAgentCommandResolverTail("foo -- note")).toEqual({
      argumentsText: "foo",
      noteText: "note",
    });
    expect(splitAgentCommandResolverTail("foo --bar")).toEqual({
      argumentsText: "foo --bar",
    });
  });

  it("expands argument commands before submitting mismatched drafts", () => {
    expect(prepareAgentCommandSubmission(agentCommandItem(), "/other")).toEqual({
      kind: "expand",
      nextDraft: "/task ",
    });
  });

  it("submits selected commands with split resolver notes", () => {
    expect(
      prepareAgentCommandSubmission(agentCommandItem(), "/task bug-123 -- ship it"),
    ).toEqual({
      argumentsText: "bug-123",
      commandName: "task",
      kind: "submit",
      noteText: "ship it",
    });
  });

  it("dispatches resolved visible and expanded prompts", () => {
    const onSend = vi.fn(() => true);

    expect(
      sendResolvedAgentCommandSubmission(onSend, "session-1", {
        expandedPrompt: null,
        kind: "promptTemplate",
        name: "task",
        source: ".termal/commands/task.md",
        visiblePrompt: "/task bug-123",
      }),
    ).toBe(true);
    expect(
      sendResolvedAgentCommandSubmission(onSend, "session-1", {
        expandedPrompt: "full prompt",
        kind: "promptTemplate",
        name: "task",
        source: ".termal/commands/task.md",
        visiblePrompt: "/task bug-456",
      }),
    ).toBe(true);

    expect(onSend).toHaveBeenNthCalledWith(1, "session-1", "/task bug-123");
    expect(onSend).toHaveBeenNthCalledWith(
      2,
      "session-1",
      "/task bug-456",
      "full prompt",
    );
  });

  it("redacts resolver errors that include local paths or secret-like text", () => {
    expect(
      formatAgentCommandResolverError(
        new ApiRequestError(
          "request-failed",
          "failed reading /Users/greg/project/.env",
          { status: 400 },
        ),
      ),
    ).toBe("Could not resolve the slash command. Check the command file and try again.");

    expect(
      formatAgentCommandResolverError(
        new Error("template failed with token=secret-value"),
      ),
    ).toBe("Could not resolve the slash command. Check the command file and try again.");
  });

  it("keeps non-sensitive resolver errors user-facing", () => {
    expect(
      formatAgentCommandResolverError(
        new ApiRequestError("request-failed", "Unknown slash command", {
          status: 400,
        }),
      ),
    ).toBe("Unknown slash command");
  });
});
