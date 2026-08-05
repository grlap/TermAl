import { describe, expect, it } from "vitest";

import { estimateConversationMessageHeight } from "./conversation-virtualization";
import {
  buildMessagePages,
  estimatePageHeight,
  pageExtendsMountedMeasurement,
} from "./virtualized-conversation-measurement";
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

function makeCommandMessage(
  overrides: Partial<Extract<Message, { type: "command" }>> = {},
) {
  return {
    id: "message-command-1",
    type: "command" as const,
    author: "assistant" as const,
    timestamp: "10:00",
    command: "npm test",
    output: "all tests passed",
    status: "success" as const,
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

  it("estimates every command status from its collapsed summary", () => {
    const shortCommand = makeCommandMessage();
    const veryLargeCommand = makeCommandMessage({
      command: Array.from({ length: 80 }, (_, index) => `echo command-${index}`).join("\n"),
      output: Array.from({ length: 700 }, (_, index) => `output-${index}`).join("\n"),
    });
    const runningCommand = makeCommandMessage({
      output: "",
      status: "running",
    });
    const failedCommand = makeCommandMessage({
      command: "long command segment ".repeat(200),
      output: Array.from({ length: 700 }, (_, index) => `error-${index}`).join("\n"),
      status: "error",
    });

    expect(estimateConversationMessageHeight(shortCommand)).toBe(120);
    expect(estimateConversationMessageHeight(veryLargeCommand)).toBe(120);
    expect(estimateConversationMessageHeight(runningCommand)).toBe(120);
    expect(
      estimateConversationMessageHeight(failedCommand, {
        availableWidthPx: 520,
      }),
    ).toBe(120);
  });

  it("keeps an unseen page of collapsed successful commands close to measured UI height", () => {
    const commands = Array.from({ length: 8 }, (_, index) =>
      makeCommandMessage({
        id: `message-command-${index}`,
        command: `command-${index}\n${"long input ".repeat(40)}`,
        output: Array.from({ length: 100 }, (_, line) => `output-${line}`).join("\n"),
      }),
    );
    const [page] = buildMessagePages(commands);
    const estimatedPageHeight = estimatePageHeight(page!, (message) =>
      estimateConversationMessageHeight(message, { availableWidthPx: 560 }),
    );
    // Live compact-density cards measure 117.7 px and this terminal page owns
    // seven 12 px gaps, for 1025.6 px total. Keep the unseen estimate within two
    // message gaps instead of the previous multi-thousand-pixel overshoot.
    expect(estimatedPageHeight).toBe(1044);
    expect(Math.abs(estimatedPageHeight - 1025.6)).toBeLessThan(20);
  });
});

describe("pageExtendsMountedMeasurement", () => {
  it("reuses a mounted page measurement only for an identity-preserving append", () => {
    const originalMessages = [
      makeTextMessage({ id: "message-1", text: "first" }),
      makeTextMessage({ id: "message-2", text: "second" }),
    ];
    const [originalPage] = buildMessagePages(originalMessages);
    const [appendedPage] = buildMessagePages([
      ...originalMessages.map((message) => ({ ...message })),
      makeTextMessage({ id: "message-3", text: "third" }),
    ]);
    const [differentPrefixPage] = buildMessagePages([
      makeTextMessage({ id: "replacement-1", text: "replaced" }),
      { ...originalMessages[1]! },
      makeTextMessage({ id: "message-3", text: "third" }),
    ]);
    const identity = {
      hasTrailingGap: originalPage!.hasTrailingGap,
      messages: originalPage!.messages,
    };

    expect(pageExtendsMountedMeasurement(appendedPage!, identity)).toBe(true);
    expect(pageExtendsMountedMeasurement(differentPrefixPage!, identity)).toBe(
      false,
    );
    expect(pageExtendsMountedMeasurement(originalPage!, identity)).toBe(false);
  });
});

describe("buildMessagePages", () => {
  it("keeps global page bands stable while a bounded tail window advances", () => {
    const messages = Array.from({ length: 20 }, (_, index) =>
      makeTextMessage({ id: `message-${100 + index}` }),
    );
    const advancedMessages = [
      ...messages.slice(1),
      makeTextMessage({ id: "message-120" }),
    ];

    const initialPages = buildMessagePages(messages, 100);
    const advancedPages = buildMessagePages(advancedMessages, 101);

    expect(initialPages.map((page) => page.key)).toEqual([
      "100:104:message-100:message-103",
      "104:112:message-104:message-111",
      "112:120:message-112:message-119",
    ]);
    expect(advancedPages.map((page) => page.key)).toEqual([
      "101:104:message-101:message-103",
      "104:112:message-104:message-111",
      "112:120:message-112:message-119",
      "120:121:message-120:message-120",
    ]);
  });
});
