import { describe, expect, it } from "vitest";

import {
  delegationWaitIndicatorPrompt,
  hasAgentOutputAfterLatestUserPrompt,
  hasTurnFinalizingOutputAfterLatestUserPrompt,
} from "./SessionPaneView.waiting-indicator";
import type { DelegationWaitRecord } from "./api";
import type { Message } from "./types";

function makeWait(
  id: string,
  overrides: Partial<DelegationWaitRecord> = {},
): DelegationWaitRecord {
  return {
    id,
    parentSessionId: "parent-session",
    delegationIds: [`delegation-${id}`],
    mode: "all",
    createdAt: "2026-05-09T00:00:00Z",
    ...overrides,
  };
}

describe("delegationWaitIndicatorPrompt", () => {
  it("includes the first title when multiple waits are pending", () => {
    expect(
      delegationWaitIndicatorPrompt([
        makeWait("one", {
          delegationIds: ["delegation-1", "delegation-2"],
          title: "review fan-in",
        }),
        makeWait("two", {
          mode: "any",
          title: "backend release gate",
        }),
      ]),
    ).toBe(
      "Waiting on 2 delegation waits covering 3 delegated sessions: review fan-in (+1 more)",
    );
  });

  it("keeps the generic multi-wait label when no wait has a title", () => {
    expect(
      delegationWaitIndicatorPrompt([
        makeWait("one", { title: "  " }),
        makeWait("two", { title: null }),
      ]),
    ).toBe("Waiting on 2 delegation waits covering 2 delegated sessions");
  });
});

describe("hasAgentOutputAfterLatestUserPrompt", () => {
  it("returns false before the optimistic send has produced agent output", () => {
    expect(
      hasAgentOutputAfterLatestUserPrompt([
        {
          id: "message-user",
          type: "text",
          author: "you",
          timestamp: "12:00",
          text: "Fix it",
        },
      ]),
    ).toBe(false);
  });

  it("returns true after the latest user prompt has a completed agent-side card", () => {
    const messages: Message[] = [
      {
        id: "message-user",
        type: "text",
        author: "you",
        timestamp: "12:00",
        text: "Fix it",
      },
      {
        id: "message-files",
        type: "fileChanges",
        author: "assistant",
        timestamp: "12:01",
        title: "Agent changed 1 file",
        files: [{ path: "ui/src/styles.css", kind: "modified" }],
      },
    ];

    expect(hasAgentOutputAfterLatestUserPrompt(messages)).toBe(true);
  });

  it("resets after a newer user prompt", () => {
    const messages: Message[] = [
      {
        id: "message-user-1",
        type: "text",
        author: "you",
        timestamp: "12:00",
        text: "First",
      },
      {
        id: "message-agent-1",
        type: "text",
        author: "assistant",
        timestamp: "12:01",
        text: "Done",
      },
      {
        id: "message-user-2",
        type: "text",
        author: "you",
        timestamp: "12:02",
        text: "Second",
      },
    ];

    expect(hasAgentOutputAfterLatestUserPrompt(messages)).toBe(false);
  });
});

describe("hasTurnFinalizingOutputAfterLatestUserPrompt", () => {
  it("returns true when the latest prompt has an agent file-change summary", () => {
    const messages: Message[] = [
      {
        id: "message-user",
        type: "text",
        author: "you",
        timestamp: "12:00",
        text: "Fix it",
      },
      {
        id: "message-files",
        type: "fileChanges",
        author: "assistant",
        timestamp: "12:01",
        title: "Agent changed 2 files",
        files: [
          { path: "ui/src/app-live-state.ts", kind: "modified" },
          { path: "ui/src/app-live-state.test.ts", kind: "modified" },
        ],
      },
    ];

    expect(hasTurnFinalizingOutputAfterLatestUserPrompt(messages)).toBe(true);
  });

  it("does not let an older file-change summary suppress a newer prompt", () => {
    const messages: Message[] = [
      {
        id: "message-user-1",
        type: "text",
        author: "you",
        timestamp: "12:00",
        text: "First",
      },
      {
        id: "message-files",
        type: "fileChanges",
        author: "assistant",
        timestamp: "12:01",
        title: "Agent changed 1 file",
        files: [{ path: "ui/src/styles.css", kind: "modified" }],
      },
      {
        id: "message-user-2",
        type: "text",
        author: "you",
        timestamp: "12:02",
        text: "Second",
      },
    ];

    expect(hasTurnFinalizingOutputAfterLatestUserPrompt(messages)).toBe(false);
  });
});
