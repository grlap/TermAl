import { describe, expect, it } from "vitest";

import {
  buildConversationOverviewProjection,
  buildConversationOverviewSegments,
  findConversationOverviewItemAtY,
  getConversationOverviewItemByMessageId,
  projectConversationOverviewViewport,
} from "./conversation-overview-map";
import type {
  VirtualizedConversationLayoutSnapshot,
  VirtualizedConversationViewportSnapshot,
} from "./VirtualizedConversationMessageList";
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

function denseAssistantOverviewProjection(messageCount: number) {
  const messages = Array.from({ length: messageCount }, (_, index) =>
    textMessage(`m${index + 1}`, {
      author: "assistant",
      text: `assistant message ${index + 1}`,
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
      endIndex: Math.min(1, messages.length - 1),
    },
    mountedPageRange: {
      startIndex: 0,
      endIndex: Math.min(1, messages.length - 1),
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

  return buildConversationOverviewProjection({
    layoutSnapshot,
    maxHeightPx: 12,
    messages,
    minItemHeightPx: 2,
  });
}

function tailWindowLayoutSnapshot({
  messages,
  sessionId = "session-1",
  startIndex,
  count,
}: {
  messages: Message[];
  sessionId?: string;
  startIndex: number;
  count: number;
}): VirtualizedConversationLayoutSnapshot {
  const tailMessages = messages.slice(startIndex, startIndex + count);
  return {
    sessionId,
    messageCount: tailMessages.length,
    estimatedTotalHeightPx: 2_000,
    viewportTopPx: 1_500,
    viewportHeightPx: 500,
    viewportWidthPx: 800,
    isActive: true,
    visiblePageRange: {
      startIndex: 15,
      endIndex: 20,
    },
    mountedPageRange: {
      startIndex: 12,
      endIndex: 20,
    },
    messages: tailMessages.map((message, index) => ({
      messageId: message.id,
      messageIndex: index,
      pageIndex: Math.floor(index / 8),
      type: message.type,
      author: message.author,
      estimatedTopPx: index * 100,
      estimatedHeightPx: 100,
      measuredPageHeightPx: null,
    })),
  };
}

function viewportSnapshotFromLayout(
  layoutSnapshot: VirtualizedConversationLayoutSnapshot,
): VirtualizedConversationViewportSnapshot {
  const firstMessage = layoutSnapshot.messages[0] ?? null;
  const lastMessage =
    layoutSnapshot.messages[layoutSnapshot.messages.length - 1] ?? null;
  return {
    sessionId: layoutSnapshot.sessionId,
    messageCount: layoutSnapshot.messageCount,
    windowStartMessageId: firstMessage?.messageId ?? null,
    windowEndMessageId: lastMessage?.messageId ?? null,
    estimatedTotalHeightPx: layoutSnapshot.estimatedTotalHeightPx,
    viewportTopPx: layoutSnapshot.viewportTopPx,
    viewportHeightPx: layoutSnapshot.viewportHeightPx,
    viewportWidthPx: layoutSnapshot.viewportWidthPx,
    isActive: layoutSnapshot.isActive,
    visiblePageRange: layoutSnapshot.visiblePageRange,
    mountedPageRange: layoutSnapshot.mountedPageRange,
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
            source: "tool",
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

  it("aligns the rendered viewport marker bottom with the rail bottom at scroll end", () => {
    const messages = Array.from({ length: 100 }, (_, index) =>
      textMessage(`m${index + 1}`, {
        text: `message ${index + 1}`,
      }),
    );
    const layoutSnapshot: VirtualizedConversationLayoutSnapshot = {
      sessionId: "session-1",
      messageCount: messages.length,
      estimatedTotalHeightPx: 1_000,
      viewportTopPx: 999,
      viewportHeightPx: 1,
      viewportWidthPx: 800,
      isActive: true,
      visiblePageRange: {
        startIndex: 99,
        endIndex: 99,
      },
      mountedPageRange: {
        startIndex: 99,
        endIndex: 99,
      },
      messages: messages.map((message, index) => ({
        messageId: message.id,
        messageIndex: index,
        pageIndex: index,
        type: message.type,
        author: message.author,
        estimatedTopPx: index * 10,
        estimatedHeightPx: 10,
        measuredPageHeightPx: null,
      })),
    };

    const projection = buildConversationOverviewProjection({
      layoutSnapshot,
      maxHeightPx: 100,
      messages,
    });

    expect(projection.totalHeightPx).toBe(100);
    expect(projection.viewportHeightPx).toBe(8);
    expect(projection.viewportTopPx).toBe(92);
    expect(projection.viewportTopPx + projection.viewportHeightPx).toBe(
      projection.totalHeightPx,
    );
  });

  it("projects a tail-window viewport against the full transcript map", () => {
    const messages = Array.from({ length: 100 }, (_, index) =>
      textMessage(`m${index + 1}`, {
        text: `message ${index + 1}`,
      }),
    );
    const tailMessages = messages.slice(-20);
    const layoutSnapshot: VirtualizedConversationLayoutSnapshot = {
      sessionId: "session-1",
      messageCount: tailMessages.length,
      estimatedTotalHeightPx: 2_000,
      viewportTopPx: 1_500,
      viewportHeightPx: 500,
      viewportWidthPx: 800,
      isActive: true,
      visiblePageRange: {
        startIndex: 15,
        endIndex: 20,
      },
      mountedPageRange: {
        startIndex: 12,
        endIndex: 20,
      },
      messages: tailMessages.map((message, index) => ({
        messageId: message.id,
        messageIndex: index,
        pageIndex: Math.floor(index / 8),
        type: message.type,
        author: message.author,
        estimatedTopPx: index * 100,
        estimatedHeightPx: 100,
        measuredPageHeightPx: null,
      })),
    };

    const projection = buildConversationOverviewProjection({
      layoutSnapshot,
      maxHeightPx: 1_000,
      messages,
    });
    const viewportProjection = projectConversationOverviewViewport(
      projection,
      layoutSnapshot,
    );
    const tailFirstItem = projection.items.find((item) => item.messageId === "m81");

    expect(tailFirstItem).not.toBeUndefined();
    expect(viewportProjection.viewportTopPx).toBeCloseTo(
      ((tailFirstItem?.documentTopPx ?? 0) + layoutSnapshot.viewportTopPx) *
        projection.scale,
    );
    expect(viewportProjection.viewportHeightPx).toBeCloseTo(
      layoutSnapshot.viewportHeightPx * projection.scale,
    );
  });

  it("does not reuse translated viewport math for a different same-size tail window", () => {
    const messages = Array.from({ length: 100 }, (_, index) =>
      textMessage(`m${index + 1}`, {
        text: `message ${index + 1}`,
      }),
    );
    const layoutSnapshot = tailWindowLayoutSnapshot({
      messages,
      startIndex: 80,
      count: 20,
    });
    const staleLayoutSnapshot = tailWindowLayoutSnapshot({
      messages,
      startIndex: 60,
      count: 20,
    });
    const projection = buildConversationOverviewProjection({
      layoutSnapshot,
      maxHeightPx: 1_000,
      messages,
    });
    const staleViewportSnapshot = viewportSnapshotFromLayout(staleLayoutSnapshot);

    const viewportProjection = projectConversationOverviewViewport(
      projection,
      staleViewportSnapshot,
    );
    const legacyViewportProjection = projectConversationOverviewViewport(
      {
        ...projection,
        viewportSnapshotTranslation: null,
      },
      staleViewportSnapshot,
    );
    const translatedViewportProjection = projectConversationOverviewViewport(
      projection,
      layoutSnapshot,
    );

    expect(viewportProjection).toEqual(legacyViewportProjection);
    expect(viewportProjection.viewportTopPx).not.toBeCloseTo(
      translatedViewportProjection.viewportTopPx,
    );
  });

  it("does not reuse translated viewport math across sessions with the same tail size", () => {
    const messages = Array.from({ length: 100 }, (_, index) =>
      textMessage(`m${index + 1}`, {
        text: `message ${index + 1}`,
      }),
    );
    const layoutSnapshot = tailWindowLayoutSnapshot({
      messages,
      sessionId: "session-1",
      startIndex: 80,
      count: 20,
    });
    const staleViewportSnapshot = {
      ...viewportSnapshotFromLayout(layoutSnapshot),
      sessionId: "session-2",
    };
    const projection = buildConversationOverviewProjection({
      layoutSnapshot,
      maxHeightPx: 1_000,
      messages,
    });

    expect(
      projectConversationOverviewViewport(projection, staleViewportSnapshot),
    ).toEqual(
      projectConversationOverviewViewport(
        {
          ...projection,
          viewportSnapshotTranslation: null,
        },
        staleViewportSnapshot,
      ),
    );
  });

  it("falls back when a tail-window layout snapshot cannot be translated", () => {
    const messages = Array.from({ length: 100 }, (_, index) =>
      textMessage(`m${index + 1}`, {
        text: `message ${index + 1}`,
      }),
    );
    const layoutSnapshot = tailWindowLayoutSnapshot({
      messages,
      startIndex: 80,
      count: 20,
    });
    const cases: Array<{
      name: string;
      layoutSnapshot: VirtualizedConversationLayoutSnapshot | null;
    }> = [
      {
        name: "missing layout snapshot",
        layoutSnapshot: null,
      },
      {
        name: "full transcript snapshot",
        layoutSnapshot: tailWindowLayoutSnapshot({
          messages,
          startIndex: 0,
          count: messages.length,
        }),
      },
      {
        name: "empty layout message window",
        layoutSnapshot: {
          ...layoutSnapshot,
          messages: [],
        },
      },
      {
        name: "first layout message missing from estimated rows",
        layoutSnapshot: {
          ...layoutSnapshot,
          messages: [
            {
              ...layoutSnapshot.messages[0]!,
              messageId: "missing-message",
            },
            ...layoutSnapshot.messages.slice(1),
          ],
        },
      },
      {
        name: "non-contiguous layout message window",
        layoutSnapshot: {
          ...layoutSnapshot,
          messages: layoutSnapshot.messages.map((message, index) =>
            index === 1 ? { ...message, messageId: "m100" } : message,
          ),
        },
      },
    ];

    cases.forEach(({ name, layoutSnapshot }) => {
      expect(
        buildConversationOverviewProjection({
          layoutSnapshot,
          maxHeightPx: 1_000,
          messages,
        }).viewportSnapshotTranslation,
        name,
      ).toBeNull();
    });
  });

  it("translates a matching live viewport snapshot with a different top offset", () => {
    const messages = Array.from({ length: 100 }, (_, index) =>
      textMessage(`m${index + 1}`, {
        text: `message ${index + 1}`,
      }),
    );
    const layoutSnapshot = tailWindowLayoutSnapshot({
      messages,
      startIndex: 80,
      count: 20,
    });
    const liveViewportSnapshot = {
      ...viewportSnapshotFromLayout(layoutSnapshot),
      viewportTopPx: 250,
    };
    const projection = buildConversationOverviewProjection({
      layoutSnapshot,
      maxHeightPx: 1_000,
      messages,
    });
    const viewportProjection = projectConversationOverviewViewport(
      projection,
      liveViewportSnapshot,
    );
    const tailFirstItem = projection.items.find((item) => item.messageId === "m81");

    expect(viewportProjection.viewportTopPx).toBeCloseTo(
      ((tailFirstItem?.documentTopPx ?? 0) + liveViewportSnapshot.viewportTopPx) *
        projection.scale,
    );
    expect(viewportProjection.viewportHeightPx).toBeCloseTo(
      liveViewportSnapshot.viewportHeightPx * projection.scale,
    );
  });

  it("falls back when a live viewport snapshot count drifts from its translation", () => {
    const messages = Array.from({ length: 100 }, (_, index) =>
      textMessage(`m${index + 1}`, {
        text: `message ${index + 1}`,
      }),
    );
    const layoutSnapshot = tailWindowLayoutSnapshot({
      messages,
      startIndex: 80,
      count: 20,
    });
    const driftedViewportSnapshot = {
      ...viewportSnapshotFromLayout(layoutSnapshot),
      messageCount: layoutSnapshot.messageCount - 1,
    };
    const projection = buildConversationOverviewProjection({
      layoutSnapshot,
      maxHeightPx: 1_000,
      messages,
    });

    expect(
      projectConversationOverviewViewport(projection, driftedViewportSnapshot),
    ).toEqual(
      projectConversationOverviewViewport(
        {
          ...projection,
          viewportSnapshotTranslation: null,
        },
        driftedViewportSnapshot,
      ),
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

  it("clumps dense mixed messages into fewer visual segments without changing hit testing", () => {
    const messages = Array.from({ length: 120 }, (_, index) =>
      textMessage(`m${index + 1}`, {
        author: index % 2 === 0 ? "you" : "assistant",
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
      maxHeightPx: 12,
      messages,
      minItemHeightPx: 2,
    });
    const segments = buildConversationOverviewSegments(projection, {
      maxItemsPerSegment: 20,
    });

    expect(segments).toHaveLength(6);
    expect(segments[0]).toEqual(
      expect.objectContaining({
        startMessageIndex: 0,
        endMessageIndex: 19,
        itemCount: 20,
        kind: "mixed",
      }),
    );
    expect(segments.length).toBeLessThan(projection.items.length);
    expect(findConversationOverviewItemAtY(projection, 7.55)?.messageId).toBe("m76");
  });

  it("caps homogeneous visual segments by maxItemsPerSegment", () => {
    const projection = denseAssistantOverviewProjection(45);
    const segments = buildConversationOverviewSegments(projection, {
      maxItemsPerSegment: 20,
    });

    expect(
      segments.map((segment) => ({
        count: segment.itemCount,
        end: segment.endMessageIndex,
        kind: segment.kind,
        start: segment.startMessageIndex,
      })),
    ).toEqual([
      { count: 20, end: 19, kind: "assistant_text", start: 0 },
      { count: 20, end: 39, kind: "assistant_text", start: 20 },
      { count: 5, end: 44, kind: "assistant_text", start: 40 },
    ]);
  });

  it("keeps exactly maxItemsPerSegment homogeneous items in one segment", () => {
    const projection = denseAssistantOverviewProjection(20);
    const segments = buildConversationOverviewSegments(projection, {
      maxItemsPerSegment: 20,
    });

    expect(segments.map((segment) => segment.itemCount)).toEqual([20]);
  });

  it("supports one-item homogeneous segment caps", () => {
    const projection = denseAssistantOverviewProjection(4);
    const segments = buildConversationOverviewSegments(projection, {
      maxItemsPerSegment: 1,
    });

    expect(
      segments.map((segment) => ({
        count: segment.itemCount,
        end: segment.endMessageIndex,
        start: segment.startMessageIndex,
      })),
    ).toEqual([
      { count: 1, end: 0, start: 0 },
      { count: 1, end: 1, start: 1 },
      { count: 1, end: 2, start: 2 },
      { count: 1, end: 3, start: 3 },
    ]);
  });

  it("keeps marker, error, and live-turn landmarks as standalone visual segments", () => {
    const messages: Message[] = [
      textMessage("m1", { text: "first" }),
      commandMessage("m2", { status: "error" }),
      textMessage("m3", { text: "marked" }),
      textMessage("m4", { text: "last" }),
    ];
    const projection = buildConversationOverviewProjection({
      maxHeightPx: 100,
      messages,
      markers: [
        {
          id: "marker-1",
          messageId: "m3",
          name: "Important",
        },
      ],
      tailItems: [
        {
          id: "live-turn:session-1",
          kind: "live_turn",
          status: "running",
          estimatedHeightPx: 20,
        },
      ],
    });
    const segments = buildConversationOverviewSegments(projection);

    expect(
      segments.map((segment) => ({
        count: segment.itemCount,
        id: projection.items[segment.startItemIndex]?.messageId,
        markerIds: segment.markerIds,
        status: segment.status,
      })),
    ).toEqual([
      { count: 1, id: "m1", markerIds: [], status: null },
      { count: 1, id: "m2", markerIds: [], status: "error" },
      { count: 1, id: "m3", markerIds: ["marker-1"], status: null },
      { count: 1, id: "m4", markerIds: [], status: null },
      {
        count: 1,
        id: "live-turn:session-1",
        markerIds: [],
        status: "running",
      },
    ]);
  });
});
