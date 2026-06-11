import { describe, expect, it } from "vitest";

import {
  CLAUDE_EFFORT_SLASH_OPTIONS,
  FALLBACK_CLAUDE_EFFORTS,
} from "./session-slash-palette";

describe("Claude effort slash choices", () => {
  it("exposes xhigh in the slash palette fallback choices", () => {
    expect(CLAUDE_EFFORT_SLASH_OPTIONS.map((option) => option.value)).toEqual([
      "default",
      "low",
      "medium",
      "high",
      "xhigh",
      "max",
    ]);
    expect(FALLBACK_CLAUDE_EFFORTS).toEqual(["low", "medium", "high", "xhigh"]);
  });
});
