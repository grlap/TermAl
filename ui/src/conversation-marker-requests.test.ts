import { describe, expect, it } from "vitest";
import { buildCreateConversationMarkerRequest } from "./conversation-marker-requests";

describe("conversation marker request builders", () => {
  it("builds the default checkpoint create request", () => {
    expect(buildCreateConversationMarkerRequest("message-1")).toEqual({
      kind: "checkpoint",
      name: "Checkpoint",
      body: null,
      color: "#3b82f6",
      messageId: "message-1",
      endMessageId: null,
    });
  });

  it("trims a custom checkpoint name and falls back when blank", () => {
    expect(
      buildCreateConversationMarkerRequest("message-1", {
        name: "  Release point  ",
      }).name,
    ).toBe("Release point");
    expect(
      buildCreateConversationMarkerRequest("message-1", { name: "   " }).name,
    ).toBe("Checkpoint");
  });
});
