import { describe, expect, it } from "vitest";

import {
  canonicalConversationMarkerColor,
  conversationMarkerColorsMatchForState,
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

  it("exposes canonical validity separately from display fallback", () => {
    expect(canonicalConversationMarkerColor("#3B82F6")).toBe("#3b82f6");
    expect(canonicalConversationMarkerColor("url(https://example.test/x)")).toBeNull();
  });

  it("matches state colors by canonical hex without equating invalid values to fallback", () => {
    expect(conversationMarkerColorsMatchForState("#3B82F6", "#3b82f6")).toBe(
      true,
    );
    expect(
      conversationMarkerColorsMatchForState(
        "url(https://example.test/x)",
        DEFAULT_CONVERSATION_MARKER_COLOR,
      ),
    ).toBe(false);
    expect(
      conversationMarkerColorsMatchForState(
        "url(https://example.test/x)",
        "url(https://example.test/x)",
      ),
    ).toBe(true);
  });
});
