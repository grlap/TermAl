import { describe, expect, it } from "vitest";
import { messageChangeMarker } from "./app-utils";
import type { ParallelAgentsMessage } from "./types";

describe("messageChangeMarker", () => {
  it("changes parallel-agent markers when only source changes", () => {
    const toolMessage: ParallelAgentsMessage = {
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
    const delegationMessage: ParallelAgentsMessage = {
      ...toolMessage,
      agents: [{ ...toolMessage.agents[0]!, source: "delegation" }],
    };

    expect(messageChangeMarker(toolMessage)).not.toBe(
      messageChangeMarker(delegationMessage),
    );
  });
});
