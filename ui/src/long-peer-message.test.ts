// Owns parity tests for the shared long-peer collapse decision.
// Does not own expanded peer rendering or virtualizer measurements.
// Split from: ui/src/MessageCard.test.tsx.

import { describe, expect, it } from "vitest";
import { shouldCollapseLongPeerMessage } from "./long-peer-message";
import type { TextMessage } from "./types";

function makeMessage(overrides: Partial<TextMessage> = {}): TextMessage {
  return {
    id: "peer-message",
    type: "text",
    author: "you",
    timestamp: "10:00",
    text: "peer result ".repeat(80),
    source: { sessionId: "session-peer", name: "Peer reviewer" },
    ...overrides,
  };
}

describe("shouldCollapseLongPeerMessage", () => {
  it("matches the renderer contract across author, source kind, and length", () => {
    expect(shouldCollapseLongPeerMessage(makeMessage())).toBe(true);
    expect(
      shouldCollapseLongPeerMessage(
        makeMessage({
          source: {
            kind: "peerBatch",
            sessionId: "session-peer",
            name: "Peer queue",
          },
        }),
      ),
    ).toBe(true);
    expect(
      shouldCollapseLongPeerMessage(makeMessage({ text: "Short peer result" })),
    ).toBe(false);
    expect(
      shouldCollapseLongPeerMessage(makeMessage({ source: null })),
    ).toBe(false);
    expect(
      shouldCollapseLongPeerMessage(makeMessage({ author: "assistant" })),
    ).toBe(false);
  });
});
