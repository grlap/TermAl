import { describe, expect, it } from "vitest";

import {
  DEFAULT_CONVERSATION_MARKER_COLOR,
  normalizeConversationMarkerColor,
} from "./conversation-marker-colors";

describe("normalizeConversationMarkerColor", () => {
  it("accepts supported hex colors and normalizes case", () => {
    expect(normalizeConversationMarkerColor(" #ABC ")).toBe("#abc");
    expect(normalizeConversationMarkerColor("#ABC8")).toBe("#abc8");
    expect(normalizeConversationMarkerColor("#3B82F6")).toBe("#3b82f6");
    expect(normalizeConversationMarkerColor("#3B82F6AA")).toBe("#3b82f6aa");
  });

  it("falls back for non-hex CSS values before they reach custom properties", () => {
    expect(normalizeConversationMarkerColor("url(https://example.test/x)")).toBe(
      DEFAULT_CONVERSATION_MARKER_COLOR,
    );
    expect(normalizeConversationMarkerColor("var(--signal-blue)")).toBe(
      DEFAULT_CONVERSATION_MARKER_COLOR,
    );
    expect(normalizeConversationMarkerColor("#12")).toBe(
      DEFAULT_CONVERSATION_MARKER_COLOR,
    );
    expect(normalizeConversationMarkerColor(null)).toBe(
      DEFAULT_CONVERSATION_MARKER_COLOR,
    );
  });
});
