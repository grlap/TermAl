// Owns: focused helper coverage for parallel-agent message cards.
// Does not own: MessageCard dispatch, delegation side effects, or message virtualization.
// Split from: ui/src/MessageCard.test.tsx.

import { describe, expect, it } from "vitest";

import {
  parallelAgentDetail,
  parallelAgentStatusLabel,
  parallelAgentStatusTone,
  parallelAgentsHeading,
  parallelAgentsSummary,
} from "./parallel-agents-card";
import type { ParallelAgentsMessage } from "./types";

function makeParallelAgentsMessage(
  agents: ParallelAgentsMessage["agents"],
): ParallelAgentsMessage {
  return {
    id: "parallel-agents-message",
    type: "parallelAgents",
    author: "assistant",
    timestamp: "10:00",
    agents,
  };
}

describe("parallel-agent card helpers", () => {
  it("summarizes active, completed, and failed agent mixes", () => {
    const message = makeParallelAgentsMessage([
      {
        id: "agent-a",
        source: "delegation",
        status: "completed",
        title: "Done",
      },
      {
        id: "agent-b",
        source: "delegation",
        status: "error",
        title: "Failed",
      },
      {
        id: "agent-c",
        source: "delegation",
        status: "running",
        title: "Running",
      },
    ]);

    expect(parallelAgentsHeading(message)).toBe("Running 3 agents");
    expect(parallelAgentsSummary(message)).toBe("1 done · 1 failed · 1 active");
  });

  it("uses singular and plural completed headings", () => {
    expect(
      parallelAgentsHeading(
        makeParallelAgentsMessage([
          {
            id: "agent-a",
            source: "delegation",
            status: "completed",
            title: "Done",
          },
        ]),
      ),
    ).toBe("1 agent completed");
    expect(
      parallelAgentsHeading(
        makeParallelAgentsMessage([
          {
            id: "agent-a",
            source: "delegation",
            status: "completed",
            title: "Done",
          },
          {
            id: "agent-b",
            source: "delegation",
            status: "completed",
            title: "Also done",
          },
        ]),
      ),
    ).toBe("2 agents completed");
  });

  it("summarizes failed-only and mixed settled states", () => {
    expect(
      parallelAgentsSummary(
        makeParallelAgentsMessage([
          {
            id: "agent-a",
            source: "delegation",
            status: "error",
            title: "Failed",
          },
        ]),
      ),
    ).toBe("1 failed");
    expect(
      parallelAgentsSummary(
        makeParallelAgentsMessage([
          {
            id: "agent-a",
            source: "delegation",
            status: "completed",
            title: "Done",
          },
          {
            id: "agent-b",
            source: "delegation",
            status: "error",
            title: "Failed",
          },
        ]),
      ),
    ).toBe("1 completed · 1 failed");
  });

  it("handles an empty agent list consistently", () => {
    const message = makeParallelAgentsMessage([]);

    expect(parallelAgentsHeading(message)).toBe("0 agents completed");
    expect(parallelAgentsSummary(message)).toBe("All task agents completed.");
  });

  it("maps status labels and tones", () => {
    expect(parallelAgentStatusLabel("initializing")).toBe("initializing");
    expect(parallelAgentStatusLabel("running")).toBe("running");
    expect(parallelAgentStatusLabel("completed")).toBe("completed");
    expect(parallelAgentStatusLabel("error")).toBe("failed");

    expect(parallelAgentStatusTone("initializing")).toBe("active");
    expect(parallelAgentStatusTone("running")).toBe("active");
    expect(parallelAgentStatusTone("completed")).toBe("idle");
    expect(parallelAgentStatusTone("error")).toBe("error");
  });

  it("uses explicit details before fallback text", () => {
    expect(
      parallelAgentDetail({
        id: "agent-a",
        source: "delegation",
        status: "running",
        title: "Running",
        detail: "Inspecting changes",
      }),
    ).toBe("Inspecting changes");
    expect(
      parallelAgentDetail({
        id: "agent-b",
        source: "delegation",
        status: "running",
        title: "Running",
      }),
    ).toBe("Initializing...");
    expect(
      parallelAgentDetail({
        id: "agent-c",
        source: "delegation",
        status: "error",
        title: "Failed",
        detail: "   ",
      }),
    ).toBe("Task failed.");
  });
});
