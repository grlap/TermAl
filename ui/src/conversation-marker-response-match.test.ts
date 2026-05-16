import { describe, expect, it } from "vitest";
import {
  conversationMarkerExactlyMatches,
  conversationMarkerSatisfiesResponse,
} from "./conversation-marker-response-match";
import type { ConversationMarker } from "./types";

function marker(overrides: Partial<ConversationMarker> = {}): ConversationMarker {
  return {
    id: "marker-1",
    sessionId: "session-1",
    kind: "checkpoint",
    name: "Checkpoint",
    body: "Body",
    color: "#ABCDEF",
    messageId: "message-1",
    messageIndexHint: 1,
    endMessageId: "message-2",
    endMessageIndexHint: 2,
    createdAt: "2026-05-16T10:00:00Z",
    updatedAt: "2026-05-16T10:00:01Z",
    createdBy: "user",
    ...overrides,
  };
}

describe("conversation marker response matching", () => {
  it("matches equivalent markers while normalizing optional empty fields and color case", () => {
    expect(
      conversationMarkerExactlyMatches(
        marker({ body: undefined, color: "#ABCDEF", endMessageId: undefined }),
        marker({ body: null, color: "#abcdef", endMessageId: null }),
      ),
    ).toBe(true);
  });

  it("rejects mismatched marker details", () => {
    expect(
      conversationMarkerExactlyMatches(
        marker(),
        marker({ updatedAt: "2026-05-16T10:00:02Z" }),
      ),
    ).toBe(false);
  });

  it("requires both stale-response operands to exist and share an id", () => {
    expect(conversationMarkerSatisfiesResponse(undefined, marker())).toBe(false);
    expect(conversationMarkerSatisfiesResponse(marker(), undefined)).toBe(false);
    expect(
      conversationMarkerSatisfiesResponse(marker(), marker({ id: "marker-2" })),
    ).toBe(false);
    expect(conversationMarkerSatisfiesResponse(marker(), marker())).toBe(true);
  });
});
