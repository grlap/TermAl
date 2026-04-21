import { describe, expect, it } from "vitest";

import { estimateConversationMessageHeight } from "./conversation-virtualization";
import type { Message } from "../types";

function makeTextMessage(overrides: Partial<Extract<Message, { type: "text" }>> = {}) {
  return {
    id: "message-1",
    type: "text" as const,
    author: "you" as const,
    timestamp: "10:00",
    text: "",
    ...overrides,
  };
}

describe("estimateConversationMessageHeight", () => {
  it("accounts for soft-wrapped long prompts instead of only explicit newlines", () => {
    const longSingleLinePrompt = "wrap me ".repeat(180).trimEnd();
    const legacyEstimate = Math.min(1800, Math.max(92, 78 + 24));

    expect(
      estimateConversationMessageHeight(
        makeTextMessage({
          text: longSingleLinePrompt,
        }),
      ),
    ).toBeGreaterThan(legacyEstimate);
  });

  it("lets genuinely tall plain-text prompts exceed the old 1800 px ceiling", () => {
    const tallPrompt = Array.from({ length: 120 }, () => "x".repeat(96)).join("\n");

    expect(
      estimateConversationMessageHeight(
        makeTextMessage({
          text: tallPrompt,
        }),
      ),
    ).toBeGreaterThan(1800);
  });

  it("reduces the estimate for wide panes and increases it for narrow panes", () => {
    const longPrompt = "scroll estimate ".repeat(220).trimEnd();

    const narrowEstimate = estimateConversationMessageHeight(
      makeTextMessage({
        text: longPrompt,
      }),
      {
        availableWidthPx: 520,
      },
    );
    const wideEstimate = estimateConversationMessageHeight(
      makeTextMessage({
        text: longPrompt,
      }),
      {
        availableWidthPx: 1280,
      },
    );

    expect(narrowEstimate).toBeGreaterThan(wideEstimate);
  });

  it("reserves extra height for prompts that render the expanded-text toggle", () => {
    const baseMessage = makeTextMessage({
      text: "Summarize this plan",
    });
    const expandedPromptMessage = makeTextMessage({
      text: "/plan",
      expandedText: "Summarize this plan",
    });

    expect(estimateConversationMessageHeight(expandedPromptMessage)).toBeGreaterThan(
      estimateConversationMessageHeight(baseMessage),
    );
  });

  it("accounts for the expanded prompt body when the panel is open", () => {
    const expandedPromptMessage = makeTextMessage({
      text: "/plan",
      expandedText: Array.from({ length: 80 }, () => "detail detail detail").join("\n"),
    });

    expect(
      estimateConversationMessageHeight(expandedPromptMessage, {
        expandedPromptOpen: true,
      }),
    ).toBeGreaterThan(
      estimateConversationMessageHeight(expandedPromptMessage, {
        expandedPromptOpen: false,
      }),
    );
  });
});
