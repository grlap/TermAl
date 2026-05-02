import { describe, expect, it } from "vitest";

import {
  buildConversationOverviewProjection,
  findConversationOverviewItemAtY,
  getConversationOverviewItemByMessageId,
} from "./conversation-overview-map";
import type { VirtualizedConversationLayoutSnapshot } from "./VirtualizedConversationMessageList";
import type { Message } from "../types";

function textMessage(
  id: string,
  overrides: Partial<Extract<Message, { type: "text" }>> = {},
): Extract<Message, { type: "text" }> {
  return {
    id,
    type: "text",
    author: "assistant",
    timestamp: "10:00",
    text: "",
    ...overrides,
  };
}

function commandMessage(
  id: string,
  overrides: Partial<Extract<Message, { type: "command" }>> = {},
): Extract<Message, { type: "command" }> {
  return {
    id,
    type: "command",
    author: "assistant",
    timestamp: "10:01",
    command: "npm test",
    output: "ok",
    status: "success",
    ...overrides,
  };
}

describe("conversation overview map", () => {
  it("classifies loaded transcript messages and projects marker pins", () => {
    const messages: Message[] = [
      textMessage("prompt", {
        author: "you",
        text: "Please summarize this very long prompt for the overview map",
      }),
      textMessage("answer", {
        author: "assistant",
        text: "The assistant response",
      }),
      commandMessage("command", {
        status: "error",
      }),
      {
        id: "agents",
        type: "parallelAgents",
        author: "assistant",
        timestamp: "10:02",
        agents: [
          {
            id: "agent-1",
            title: "Review",
            status: "running",
          },
        ],
      },
      {
        id: "approval",
        type: "approval",
        author: "assistant",
        timestamp: "10:03",
        title: "Approve command",
        command: "git status",
        detail: "Needs permission",
        decision: "pending",
      },
      {
        id: "files",
        type: "fileChanges",
        author: "assistant",
        timestamp: "10:04",
        title: "Changed files",
        files: [
          {
            path: "src/main.rs",
            kind: "modified",
          },
        ],
      },
    ];

    const projection = buildConversationOverviewProjection({
      maxSampleLength: 20,
      messages,
      markers: [
        {
          id: "marker-1",
          messageId: "answer",
          name: "Decision point",
          color: "#38bdf8",
          kind: "note",
        },
      ],
    });

    expect(projection.items.map((item) => item.kind)).toEqual([
      "user_prompt",
      "assistant_text",
      "command",
      "parallel_agents",
      "approval",
      "file_changes",
    ]);
    expect(projection.items.map((item) => item.status)).toEqual([
      null,
      null,
      "error",
      "running",
      "approval",
      null,
    ]);
    expect(projection.items[0]?.textSample).toBe("Please summarize...");
    expect(projection.markers).toEqual([
      expect.objectContaining({
        id: "marker-1",
        itemIndex: 1,
        messageId: "answer",
        name: "Decision point",
      }),
    ]);
    expect(projection.items[1]?.markerIds).toEqual(["marker-1"]);
  });

  it("uses virtualizer layout and page measurements to refine source geometry", () => {
    const messages: Message[] = [
      textMessage("m1", {
        text: "first",
      }),
      textMessage("m2", {
        text: "second",
      }),
      textMessage("m3", {
        text: "third",
      }),
    ];
    const layoutSnapshot: VirtualizedConversationLayoutSnapshot = {
      sessionId: "session-1",
      messageCount: 3,
      estimatedTotalHeightPx: 700,
      viewportTopPx: 350,
      viewportHeightPx: 140,
      viewportWidthPx: 800,
      isActive: true,
      visiblePageRange: {
        startIndex: 0,
        endIndex: 1,
      },
      mountedPageRange: {
        startIndex: 0,
        endIndex: 1,
      },
      messages: [
        {
          messageId: "m1",
          messageIndex: 0,
          pageIndex: 0,
          type: "text",
          author: "assistant",
          estimatedTopPx: 0,
          estimatedHeightPx: 100,
          measuredPageHeightPx: 600,
        },
        {
          messageId: "m2",
          messageIndex: 1,
          pageIndex: 0,
          type: "text",
          author: "assistant",
          estimatedTopPx: 100,
          estimatedHeightPx: 200,
          measuredPageHeightPx: 600,
        },
        {
          messageId: "m3",
          messageIndex: 2,
          pageIndex: 1,
          type: "text",
          author: "assistant",
          estimatedTopPx: 300,
          estimatedHeightPx: 400,
          measuredPageHeightPx: null,
        },
      ],
    };

    const projection = buildConversationOverviewProjection({
      layoutSnapshot,
      maxHeightPx: 500,
      messages,
    });

    expect(projection.sourceHeightPx).toBe(1000);
    expect(projection.totalHeightPx).toBe(500);
    expect(projection.scale).toBe(0.5);
    expect(projection.viewportTopPx).toBe(250);
    expect(projection.viewportHeightPx).toBe(100);
    expect(projection.items[0]).toEqual(
      expect.objectContaining({
        estimatedTopPx: 0,
        estimatedHeightPx: 100,
        measuredHeightPx: 200,
        measuredPageHeightPx: 600,
        documentTopPx: 0,
        documentHeightPx: 200,
        mapTopPx: 0,
        mapHeightPx: 100,
      }),
    );
    expect(projection.items[1]).toEqual(
      expect.objectContaining({
        estimatedTopPx: 100,
        measuredHeightPx: 400,
        documentTopPx: 200,
        documentHeightPx: 400,
        mapTopPx: 100,
        mapHeightPx: 200,
      }),
    );
    expect(projection.items[2]).toEqual(
      expect.objectContaining({
        estimatedTopPx: 300,
        measuredHeightPx: null,
        documentTopPx: 600,
        documentHeightPx: 400,
        mapTopPx: 300,
        mapHeightPx: 200,
      }),
    );
  });

  it("resolves map coordinates and message ids for navigation intents", () => {
    const projection = buildConversationOverviewProjection({
      maxHeightPx: 480,
      messages: [
        textMessage("m1", {
          text: "first",
        }),
        commandMessage("m2", {
          output: Array.from({ length: 20 }, (_, index) => `line ${index}`).join("\n"),
        }),
        textMessage("m3", {
          text: "third",
        }),
      ],
    });
    const secondItem = getConversationOverviewItemByMessageId(projection, "m2");

    expect(secondItem).not.toBeNull();
    expect(getConversationOverviewItemByMessageId(projection, "missing")).toBeNull();
    expect(findConversationOverviewItemAtY(projection, -100)?.messageId).toBe("m1");
    expect(
      findConversationOverviewItemAtY(
        projection,
        (secondItem?.mapTopPx ?? 0) + (secondItem?.mapHeightPx ?? 0) / 2,
      )?.messageId,
    ).toBe("m2");
    expect(findConversationOverviewItemAtY(projection, 999_999)?.messageId).toBe("m3");
  });

  it("projects live-turn tail items after loaded transcript messages", () => {
    const projection = buildConversationOverviewProjection({
      maxHeightPx: 480,
      messages: [
        textMessage("m1", {
          text: "first",
        }),
      ],
      tailItems: [
        {
          id: "live-turn:session-1",
          kind: "live_turn",
          status: "running",
          estimatedHeightPx: 120,
          textSample: "Codex is working",
        },
      ],
    });

    expect(projection.items).toHaveLength(2);
    expect(projection.items[1]).toEqual(
      expect.objectContaining({
        messageId: "live-turn:session-1",
        messageIndex: 1,
        type: null,
        kind: "live_turn",
        status: "running",
        textSample: "Codex is working",
      }),
    );
    expect(findConversationOverviewItemAtY(projection, 999_999)?.messageId).toBe(
      "live-turn:session-1",
    );
  });

  it("keeps the viewport marker inside the rail when the live-turn tail extends scroll height", () => {
    const messages = [
      textMessage("m1", { text: "first" }),
      textMessage("m2", { text: "second" }),
    ];
    const layoutSnapshot: VirtualizedConversationLayoutSnapshot = {
      sessionId: "session-1",
      messageCount: messages.length,
      estimatedTotalHeightPx: 200,
      viewportTopPx: 190,
      viewportHeightPx: 100,
      viewportWidthPx: 800,
      isActive: true,
      visiblePageRange: {
        startIndex: 0,
        endIndex: 1,
      },
      mountedPageRange: {
        startIndex: 0,
        endIndex: 1,
      },
      messages: messages.map((message, index) => ({
        messageId: message.id,
        messageIndex: index,
        pageIndex: index,
        type: message.type,
        author: message.author,
        estimatedTopPx: index * 100,
        estimatedHeightPx: 100,
        measuredPageHeightPx: null,
      })),
    };

    const projection = buildConversationOverviewProjection({
      layoutSnapshot,
      maxHeightPx: 140,
      messages,
      tailItems: [
        {
          id: "live-turn:session-1",
          kind: "live_turn",
          status: "running",
          estimatedHeightPx: 80,
          textSample: "Codex is working",
        },
      ],
    });

    expect(projection.viewportTopPx + projection.viewportHeightPx).toBeLessThanOrEqual(
      projection.totalHeightPx,
    );
  });

  it("hit-tests against true scaled bounds when visual minimum heights overlap", () => {
    const messages = Array.from({ length: 10 }, (_, index) =>
      textMessage(`m${index + 1}`, {
        text: `message ${index + 1}`,
      }),
    );
    const layoutSnapshot: VirtualizedConversationLayoutSnapshot = {
      sessionId: "session-1",
      messageCount: messages.length,
      estimatedTotalHeightPx: messages.length,
      viewportTopPx: 0,
      viewportHeightPx: 1,
      viewportWidthPx: 800,
      isActive: true,
      visiblePageRange: {
        startIndex: 0,
        endIndex: 1,
      },
      mountedPageRange: {
        startIndex: 0,
        endIndex: 1,
      },
      messages: messages.map((message, index) => ({
        messageId: message.id,
        messageIndex: index,
        pageIndex: index,
        type: message.type,
        author: message.author,
        estimatedTopPx: index,
        estimatedHeightPx: 1,
        measuredPageHeightPx: null,
      })),
    };

    const projection = buildConversationOverviewProjection({
      layoutSnapshot,
      maxHeightPx: 1,
      messages,
      minItemHeightPx: 2,
    });

    expect(projection.items[0]?.mapHeightPx).toBe(2);
    expect(projection.items[9]?.mapTopPx).toBeCloseTo(0.9);
    expect(findConversationOverviewItemAtY(projection, 0.95)?.messageId).toBe("m10");
  });
});
