// Pins pure transcript transition accumulation and classification.
// Does not exercise React lifecycle or DOM scroll side effects.

import { describe, expect, it } from "vitest";

import {
  buildTurnContentTransition,
  classifyTurnContentTransition,
  didLatestTurnContentChangeBeyondPromptResidency,
} from "./SessionPaneView.content-transition";

describe("turn content transitions", () => {
  it("retains an unconsumed history transition across an inactive no-change render", () => {
    const historyTransition = buildTurnContentTransition({
      lastConsumedMessageContentSignature: "tail",
      latestTurnChangedBeyondPromptResidency: false,
      pendingPromptsAdvanced: false,
      previousMessageContentSignature: "tail",
      previousTransition: undefined,
      tailMessageChanged: false,
      toMessageContentSignature: "history|tail",
    });
    const reactivationTransition = buildTurnContentTransition({
      lastConsumedMessageContentSignature: "tail",
      latestTurnChangedBeyondPromptResidency: false,
      pendingPromptsAdvanced: false,
      previousMessageContentSignature: "history|tail",
      previousTransition: historyTransition,
      tailMessageChanged: false,
      toMessageContentSignature: "history|tail",
    });

    expect(reactivationTransition.fromMessageContentSignature).toBe("tail");
    expect(
      classifyTurnContentTransition({
        currentMessageContentSignature: "history|tail",
        previousMessageContentSignature: "tail",
        showWaitingIndicator: false,
        transition: reactivationTransition,
      }),
    ).toBe("residentHistoryOnly");
  });

  it("preserves genuine tail and queued-prompt activity while accumulating", () => {
    const transition = buildTurnContentTransition({
      lastConsumedMessageContentSignature: "prompt",
      latestTurnChangedBeyondPromptResidency: true,
      pendingPromptsAdvanced: true,
      previousMessageContentSignature: "prompt",
      previousTransition: undefined,
      tailMessageChanged: true,
      toMessageContentSignature: "prompt|reply",
    });

    expect(
      classifyTurnContentTransition({
        currentMessageContentSignature: "prompt|reply",
        previousMessageContentSignature: "prompt",
        showWaitingIndicator: true,
        transition,
      }),
    ).toBe("live");
  });

  it("classifies queued prompts ahead of simultaneous resident-history changes", () => {
    const transition = buildTurnContentTransition({
      lastConsumedMessageContentSignature: "tail",
      latestTurnChangedBeyondPromptResidency: false,
      pendingPromptsAdvanced: true,
      previousMessageContentSignature: "tail",
      previousTransition: undefined,
      tailMessageChanged: false,
      toMessageContentSignature: "prompt|tail",
    });

    expect(
      classifyTurnContentTransition({
        currentMessageContentSignature: "prompt|tail",
        previousMessageContentSignature: "tail",
        showWaitingIndicator: true,
        transition,
      }),
    ).toBe("pendingPromptsAdvanced");
  });

  it("lets later mid-turn activity outrank an accumulated history change", () => {
    const historyTransition = buildTurnContentTransition({
      lastConsumedMessageContentSignature: "tail",
      latestTurnChangedBeyondPromptResidency: false,
      pendingPromptsAdvanced: false,
      previousMessageContentSignature: "tail",
      previousTransition: undefined,
      tailMessageChanged: false,
      toMessageContentSignature: "prompt|tail",
    });
    const liveTransition = buildTurnContentTransition({
      lastConsumedMessageContentSignature: "tail",
      latestTurnChangedBeyondPromptResidency: true,
      pendingPromptsAdvanced: false,
      previousMessageContentSignature: "prompt|tail",
      previousTransition: historyTransition,
      tailMessageChanged: false,
      toMessageContentSignature: "prompt|command-output|tail",
    });

    expect(liveTransition.hasLiveLatestTurnChange).toBe(true);
    expect(
      classifyTurnContentTransition({
        currentMessageContentSignature: "prompt|command-output|tail",
        previousMessageContentSignature: "tail",
        showWaitingIndicator: false,
        transition: liveTransition,
      }),
    ).toBe("live");
  });

  it("detects same-render mid-turn progress across a prompt residency change", () => {
    const changedBeyondResidency =
      didLatestTurnContentChangeBeyondPromptResidency({
        currentMessages: [
          {
            id: "prompt",
            type: "text",
            author: "you",
            text: "prompt",
            timestamp: "10:00",
          },
          {
            id: "command",
            type: "command",
            author: "assistant",
            command: "build",
            output: "",
            status: "running",
            timestamp: "10:01",
          },
          {
            id: "new-output",
            type: "text",
            author: "assistant",
            text: "new",
            timestamp: "10:02",
          },
          {
            id: "tail",
            type: "text",
            author: "assistant",
            text: "stable",
            timestamp: "10:03",
          },
        ],
        currentPromptMessageId: "prompt",
        latestTurnChanged: true,
        previousMessages: [
          {
            id: "command",
            type: "command",
            author: "assistant",
            command: "build",
            output: "",
            status: "running",
            timestamp: "10:01",
          },
          {
            id: "tail",
            type: "text",
            author: "assistant",
            text: "stable",
            timestamp: "10:03",
          },
        ],
        previousPromptMessageId: null,
        promptResidencyChanged: true,
      });
    const transition = buildTurnContentTransition({
      lastConsumedMessageContentSignature: "tail",
      latestTurnChangedBeyondPromptResidency: changedBeyondResidency,
      pendingPromptsAdvanced: false,
      previousMessageContentSignature: "tail",
      previousTransition: undefined,
      tailMessageChanged: false,
      toMessageContentSignature: "prompt|command-output|tail",
    });

    expect(changedBeyondResidency).toBe(true);
    expect(
      classifyTurnContentTransition({
        currentMessageContentSignature: "prompt|command-output|tail",
        previousMessageContentSignature: "tail",
        showWaitingIndicator: false,
        transition,
      }),
    ).toBe("live");
  });

  it("keeps a prefix-only prompt reveal classified as resident history", () => {
    expect(
      didLatestTurnContentChangeBeyondPromptResidency({
        currentMessages: [
          {
            id: "prompt",
            type: "text",
            author: "you",
            text: "prompt",
            timestamp: "10:00",
          },
          {
            id: "revealed-output",
            type: "text",
            author: "assistant",
            text: "older",
            timestamp: "10:01",
          },
          {
            id: "tail",
            type: "text",
            author: "assistant",
            text: "stable",
            timestamp: "10:02",
          },
        ],
        currentPromptMessageId: "prompt",
        latestTurnChanged: true,
        previousMessages: [
          {
            id: "tail",
            type: "text",
            author: "assistant",
            text: "stable",
            timestamp: "10:02",
          },
        ],
        previousPromptMessageId: null,
        promptResidencyChanged: true,
      }),
    ).toBe(false);
  });

  it("detects an equal-length text replacement during a prompt reveal", () => {
    const previousTail = {
      id: "tail",
      type: "text" as const,
      author: "assistant" as const,
      text: "old",
      timestamp: "10:02",
    };

    expect(
      didLatestTurnContentChangeBeyondPromptResidency({
        currentMessages: [
          {
            id: "prompt",
            type: "text",
            author: "you",
            text: "prompt",
            timestamp: "10:00",
          },
          { ...previousTail, text: "new" },
        ],
        currentPromptMessageId: "prompt",
        // The ordinary length-based signature cannot see this replacement.
        latestTurnChanged: false,
        previousMessages: [previousTail],
        previousPromptMessageId: null,
        promptResidencyChanged: true,
      }),
    ).toBe(true);
  });

  it("detects interaction-state activity during a prompt reveal", () => {
    const pendingRequest = {
      id: "request",
      type: "userInputRequest" as const,
      author: "assistant" as const,
      timestamp: "10:02",
      title: "Choose",
      detail: "Pick one",
      questions: [],
      state: "pending" as const,
      declinable: false,
    };

    expect(
      didLatestTurnContentChangeBeyondPromptResidency({
        currentMessages: [
          {
            id: "prompt",
            type: "text",
            author: "you",
            text: "prompt",
            timestamp: "10:00",
          },
          { ...pendingRequest, state: "submitted" },
        ],
        currentPromptMessageId: "prompt",
        // The boundary comparison remains content-sensitive even if a caller
        // arrives with a stale or summarized latest-turn change signal.
        latestTurnChanged: false,
        previousMessages: [pendingRequest],
        previousPromptMessageId: null,
        promptResidencyChanged: true,
      }),
    ).toBe(true);
  });

  it("ignores object-key insertion order during a prompt reveal", () => {
    const previousTail = {
      id: "tail",
      type: "text" as const,
      timestamp: "10:02",
      author: "assistant" as const,
      text: "stable",
    };
    const currentTail = {
      text: "stable",
      author: "assistant" as const,
      timestamp: "10:02",
      type: "text" as const,
      id: "tail",
    };

    expect(
      didLatestTurnContentChangeBeyondPromptResidency({
        currentMessages: [
          {
            id: "prompt",
            type: "text",
            author: "you",
            text: "prompt",
            timestamp: "10:00",
          },
          currentTail,
        ],
        currentPromptMessageId: "prompt",
        latestTurnChanged: true,
        previousMessages: [previousTail],
        previousPromptMessageId: null,
        promptResidencyChanged: true,
      }),
    ).toBe(false);
  });

  it("fails open when a residency transition has no prior message baseline", () => {
    expect(
      didLatestTurnContentChangeBeyondPromptResidency({
        currentMessages: [],
        currentPromptMessageId: "prompt",
        latestTurnChanged: false,
        previousMessages: undefined,
        previousPromptMessageId: null,
        promptResidencyChanged: true,
      }),
    ).toBe(true);
  });

  it("detects an insertion when the prompt is trimmed in the same render", () => {
    const prompt = {
      id: "prompt",
      type: "text" as const,
      author: "you" as const,
      text: "prompt",
      timestamp: "10:00",
    };
    const command = {
      id: "command",
      type: "command" as const,
      author: "assistant" as const,
      command: "build",
      output: "",
      status: "running" as const,
      timestamp: "10:01",
    };
    const inserted = {
      id: "new-output",
      type: "text" as const,
      author: "assistant" as const,
      text: "new",
      timestamp: "10:02",
    };
    const tail = {
      id: "tail",
      type: "text" as const,
      author: "assistant" as const,
      text: "stable",
      timestamp: "10:03",
    };

    expect(
      didLatestTurnContentChangeBeyondPromptResidency({
        currentMessages: [command, inserted, tail],
        currentPromptMessageId: null,
        latestTurnChanged: true,
        previousMessages: [prompt, command, tail],
        previousPromptMessageId: "prompt",
        promptResidencyChanged: true,
      }),
    ).toBe(true);
  });

  it("keeps a suffix-only prompt trim classified as resident history", () => {
    const prompt = {
      id: "prompt",
      type: "text" as const,
      author: "you" as const,
      text: "prompt",
      timestamp: "10:00",
    };
    const revealed = {
      id: "revealed-output",
      type: "text" as const,
      author: "assistant" as const,
      text: "older",
      timestamp: "10:01",
    };
    const tail = {
      id: "tail",
      type: "text" as const,
      author: "assistant" as const,
      text: "stable",
      timestamp: "10:02",
    };

    expect(
      didLatestTurnContentChangeBeyondPromptResidency({
        currentMessages: [tail],
        currentPromptMessageId: null,
        latestTurnChanged: true,
        previousMessages: [prompt, revealed, tail],
        previousPromptMessageId: "prompt",
        promptResidencyChanged: true,
      }),
    ).toBe(false);
  });

  it("short-circuits ordinary no-change and live transitions", () => {
    expect(
      didLatestTurnContentChangeBeyondPromptResidency({
        currentMessages: [],
        currentPromptMessageId: null,
        latestTurnChanged: false,
        previousMessages: [],
        previousPromptMessageId: null,
        promptResidencyChanged: false,
      }),
    ).toBe(false);
    expect(
      didLatestTurnContentChangeBeyondPromptResidency({
        currentMessages: [],
        currentPromptMessageId: null,
        latestTurnChanged: true,
        previousMessages: [],
        previousPromptMessageId: null,
        promptResidencyChanged: false,
      }),
    ).toBe(true);
  });

  it("rejects a transition that does not describe the consumer signature pair", () => {
    expect(
      classifyTurnContentTransition({
        currentMessageContentSignature: "current",
        previousMessageContentSignature: "consumer-baseline",
        showWaitingIndicator: false,
        transition: {
          fromMessageContentSignature: "different-baseline",
          hasLiveLatestTurnChange: false,
          pendingPromptsAdvanced: false,
          tailMessageChanged: false,
          toMessageContentSignature: "current",
        },
      }),
    ).toBe("live");
  });
});
