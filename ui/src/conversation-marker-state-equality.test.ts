import { describe, expect, it } from "vitest";

import { DEFAULT_CONVERSATION_MARKER_COLOR } from "./conversation-marker-colors";
import { conversationMarkerColorsMatchForState } from "./conversation-marker-state-equality";

describe("conversationMarkerColorsMatchForState", () => {
  it("matches valid colors by canonical hex", () => {
    expect(conversationMarkerColorsMatchForState("#3B82F6", "#3b82f6")).toBe(
      true,
    );
  });

  it("does not equate invalid values to the display fallback", () => {
    expect(
      conversationMarkerColorsMatchForState(
        "url(https://example.test/x)",
        DEFAULT_CONVERSATION_MARKER_COLOR,
      ),
    ).toBe(false);
  });

  it("compares invalid values by raw identity only", () => {
    expect(
      conversationMarkerColorsMatchForState(
        "url(https://example.test/x)",
        "url(https://example.test/x)",
      ),
    ).toBe(true);
    expect(conversationMarkerColorsMatchForState("url(a)", "url(b)")).toBe(
      false,
    );
    expect(conversationMarkerColorsMatchForState(null, "url(a)")).toBe(false);
  });
});
