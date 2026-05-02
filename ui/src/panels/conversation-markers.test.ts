import { describe, expect, it } from "vitest";

import {
  findMountedConversationMessageSlot,
  groupConversationMarkersByMessageId,
  sortConversationMarkersForNavigation,
} from "./conversation-markers";
import type { ConversationMarker, Message } from "../types";

function makeMessage(id: string): Message {
  return {
    id,
    timestamp: "2026-05-02 10:00:00",
    author: "assistant",
    type: "text",
    text: `Message ${id}`,
  };
}

function makeMarker(
  id: string,
  overrides: Partial<ConversationMarker> = {},
): ConversationMarker {
  return {
    id,
    sessionId: "session-1",
    kind: "custom",
    name: `Marker ${id}`,
    body: null,
    color: "#8ab4f8",
    messageId: "message-1",
    messageIndexHint: 0,
    endMessageId: null,
    endMessageIndexHint: null,
    createdAt: "2026-05-02 10:00:00",
    updatedAt: "2026-05-02 10:00:00",
    createdBy: "user",
    ...overrides,
  };
}

describe("conversation marker helpers", () => {
  it("groups markers by message id without reordering markers inside a group", () => {
    const first = makeMarker("marker-1", { messageId: "message-1" });
    const second = makeMarker("marker-2", { messageId: "message-2" });
    const third = makeMarker("marker-3", { messageId: "message-1" });

    const grouped = groupConversationMarkersByMessageId([first, second, third]);

    expect(grouped.get("message-1")).toEqual([first, third]);
    expect(grouped.get("message-2")).toEqual([second]);
  });

  it("finds mounted message slots by search item key within the supplied root", () => {
    const root = document.createElement("section");
    const outside = document.createElement("div");
    const matching = document.createElement("article");
    const unrelated = document.createElement("article");

    outside.dataset.sessionSearchItemKey = "message:message-1";
    matching.dataset.sessionSearchItemKey = "message:message-1";
    unrelated.dataset.sessionSearchItemKey = "message:message-2";
    root.append(unrelated, matching);
    document.body.append(outside);

    try {
      expect(findMountedConversationMessageSlot("message-1", root)).toBe(
        matching,
      );
      expect(findMountedConversationMessageSlot("message-2", root)).toBe(
        unrelated,
      );
      expect(findMountedConversationMessageSlot("missing", root)).toBeNull();
    } finally {
      outside.remove();
    }
  });

  it("sorts markers by mounted message order before persisted index hints", () => {
    const messages = [makeMessage("message-1"), makeMessage("message-2")];
    const later = makeMarker("marker-1", {
      messageId: "message-2",
      messageIndexHint: 0,
    });
    const earlier = makeMarker("marker-2", {
      messageId: "message-1",
      messageIndexHint: 99,
    });

    expect(
      sortConversationMarkersForNavigation([later, earlier], messages),
    ).toEqual([earlier, later]);
  });

  it("falls back to index hints, creation time, and id for stable navigation", () => {
    const newest = makeMarker("marker-c", {
      messageId: "missing-1",
      messageIndexHint: 5,
      createdAt: "2026-05-02 10:00:02",
    });
    const oldestById = makeMarker("marker-a", {
      messageId: "missing-2",
      messageIndexHint: 5,
      createdAt: "2026-05-02 10:00:01",
    });
    const oldestByIdAfter = makeMarker("marker-b", {
      messageId: "missing-3",
      messageIndexHint: 5,
      createdAt: "2026-05-02 10:00:01",
    });
    const earliestHint = makeMarker("marker-d", {
      messageId: "missing-4",
      messageIndexHint: 1,
      createdAt: "2026-05-02 10:00:03",
    });

    expect(
      sortConversationMarkersForNavigation(
        [newest, oldestByIdAfter, earliestHint, oldestById],
        [],
      ),
    ).toEqual([earliestHint, oldestById, oldestByIdAfter, newest]);
  });
});
