// Owns parity tests for the shared delegation fan-in collapse decision.
// Does not own card rendering, expansion state, or virtualizer measurement.
// Split from: ui/src/MessageCard.test.tsx.

import { describe, expect, it } from "vitest";
import { shouldCollapseDelegationFanInMessage } from "./delegation-fan-in";
import type { TextMessage } from "./types";

const FAN_IN_TEXT = [
  "Codex and Claude /review-code",
  "",
  "Wait id: `delegation-wait-123`",
  "",
  "Delegations:",
  "- reviewer-a",
  "",
  "Results:",
  "### reviewer-a",
  "No findings.",
].join("\n");

function makeMessage(overrides: Partial<TextMessage> = {}): TextMessage {
  return {
    id: "fan-in",
    type: "text",
    author: "you",
    timestamp: "10:00",
    text: FAN_IN_TEXT,
    ...overrides,
  };
}

describe("shouldCollapseDelegationFanInMessage", () => {
  it("matches the renderer contract across text shape, author, and source", () => {
    expect(shouldCollapseDelegationFanInMessage(makeMessage())).toBe(true);
    expect(
      shouldCollapseDelegationFanInMessage(
        makeMessage({ text: "Please review these delegation results." }),
      ),
    ).toBe(false);
    expect(
      shouldCollapseDelegationFanInMessage(
        makeMessage({ author: "assistant" }),
      ),
    ).toBe(false);
    expect(
      shouldCollapseDelegationFanInMessage(
        makeMessage({
          source: { sessionId: "session-peer", name: "Peer reviewer" },
        }),
      ),
    ).toBe(false);
  });
});
