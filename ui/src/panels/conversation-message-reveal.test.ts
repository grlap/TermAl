import { render } from "@testing-library/react";
import { createElement } from "react";
import { describe, expect, it } from "vitest";

import {
  resolveConversationMessageRevealTransition,
  useConversationMessageRevealIds,
  useConversationMessageRevealOnMount,
  type ConversationMessageRevealInput,
  type ConversationMessageRevealState,
} from "./conversation-message-reveal";
import type { Message } from "../types";

const input = (
  messageIds: readonly string[],
  overrides: Partial<ConversationMessageRevealInput> = {},
): ConversationMessageRevealInput => ({
  isActive: true,
  liveTurnVisible: false,
  messageIds,
  pendingPromptIds: [],
  userScrollGeneration: 0,
  ...overrides,
});

function advance(
  previous: ConversationMessageRevealState | null,
  current: ConversationMessageRevealInput,
) {
  return resolveConversationMessageRevealTransition(previous, current);
}

describe("conversation message reveal continuity", () => {
  it("seeds an initial resident window without revealing hydrated content", () => {
    const transition = advance(null, input(["message-1", "message-2"]));

    expect(transition.revealMessageIds).toEqual([]);
    expect(transition.nextState.tailMessageId).toBe("message-2");
  });

  it("reveals every genuinely appended id in one bulk commit", () => {
    const seeded = advance(null, input(["message-1"]));
    const appended = advance(
      seeded.nextState,
      input(["message-1", "message-2", "message-3"]),
    );

    expect(appended.revealMessageIds).toEqual(["message-2", "message-3"]);
    expect([...appended.nextState.pendingRevealIds.keys()]).toEqual([
      "message-2",
      "message-3",
    ]);
  });

  it("does not reveal a streaming update to an existing message id", () => {
    const seeded = advance(null, input(["message-1", "message-live"]));
    const updated = advance(
      seeded.nextState,
      input(["message-1", "message-live"]),
    );

    expect(updated.revealMessageIds).toEqual([]);
  });

  it("does not reveal prepended history and keeps the tail identity", () => {
    const seeded = advance(null, input(["message-100", "message-101"]));
    const prepended = advance(
      seeded.nextState,
      input(["message-98", "message-99", "message-100", "message-101"]),
    );

    expect(prepended.revealMessageIds).toEqual([]);
    expect(prepended.nextState.tailMessageId).toBe("message-101");
  });

  it("does not replay on remount, recycling, or a disjoint adopted window", () => {
    const seeded = advance(null, input(["message-100", "message-101"]));
    const remounted = advance(
      seeded.nextState,
      input(["message-100", "message-101"]),
    );
    const adopted = advance(
      remounted.nextState,
      input(["message-400", "message-401"]),
    );

    expect(remounted.revealMessageIds).toEqual([]);
    expect(adopted.revealMessageIds).toEqual([]);
  });

  it("does not reveal the first bounded-tail hydration after an empty summary", () => {
    const summary = advance(null, input([]));
    const hydrated = advance(
      summary.nextState,
      input(["message-3028", "message-3029", "message-3030"]),
    );

    expect(hydrated.revealMessageIds).toEqual([]);
  });

  it("reveals an append when a bounded tail drops its oldest resident id", () => {
    const initialIds = Array.from(
      { length: 21 },
      (_, index) => `message-${3028 + index}`,
    );
    const seeded = advance(null, input(initialIds));
    const appended = advance(
      seeded.nextState,
      input([...initialIds.slice(1), "message-3049"]),
    );

    expect(appended.revealMessageIds).toEqual(["message-3049"]);
  });

  it("ignores a metadata-only count phase before revealing the appended id", () => {
    const seeded = advance(null, input(["message-1", "message-2"]));
    const metadataOnly = advance(
      seeded.nextState,
      input(["message-1", "message-2"]),
    );
    const appended = advance(
      metadataOnly.nextState,
      input(["message-1", "message-2", "message-3"]),
    );

    expect(metadataOnly.revealMessageIds).toEqual([]);
    expect(appended.revealMessageIds).toEqual(["message-3"]);
  });

  it("does not reveal a final message that replaces the live-turn surface", () => {
    const live = advance(
      null,
      input(["message-1"], { liveTurnVisible: true }),
    );
    const completed = advance(
      live.nextState,
      input(["message-1", "message-final"], { liveTurnVisible: false }),
    );
    const later = advance(
      completed.nextState,
      input(["message-1", "message-final", "message-later"]),
    );

    expect(completed.revealMessageIds).toEqual([]);
    expect(later.revealMessageIds).toEqual(["message-later"]);
  });

  it("does not reveal a queued prompt when the same id becomes resident", () => {
    const queued = advance(
      null,
      input(["message-1"], { pendingPromptIds: ["prompt-queued"] }),
    );
    const delivered = advance(
      queued.nextState,
      input(["message-1", "prompt-queued"]),
    );

    expect(delivered.revealMessageIds).toEqual([]);
  });

  it("advances the tail identity without revealing while the page is inactive", () => {
    const seeded = advance(null, input(["message-1"]));
    const hiddenAppend = advance(
      seeded.nextState,
      input(["message-1", "message-2"], { isActive: false }),
    );
    const activated = advance(
      hiddenAppend.nextState,
      input(["message-1", "message-2"]),
    );

    expect(hiddenAppend.revealMessageIds).toEqual([]);
    expect(activated.revealMessageIds).toEqual([]);
  });
});

function makeMessages(messageIds: readonly string[]): Message[] {
  return messageIds.map((id) => ({
    id,
    type: "text",
    timestamp: "10:00",
    author: "assistant",
    text: id,
  }));
}

function RevealMount({
  messageId,
  revealInCurrentCommit,
  revealUserScrollGeneration,
  sessionId,
  userScrollGeneration,
}: {
  messageId: string;
  revealInCurrentCommit: boolean;
  revealUserScrollGeneration: number;
  sessionId: string;
  userScrollGeneration: number;
}) {
  const shouldReveal = useConversationMessageRevealOnMount({
    messageId,
    revealInCurrentCommit,
    revealUserScrollGeneration,
    sessionId,
    userScrollGeneration,
  });
  return createElement("div", {
    "data-testid": "reveal-probe",
    "data-will-reveal": shouldReveal ? "true" : "false",
  });
}

function DelayedMountHarness({
  messageIds,
  mountTail,
  sessionId,
  userScrollGeneration,
}: {
  messageIds: readonly string[];
  mountTail: boolean;
  sessionId: string;
  userScrollGeneration: number;
}) {
  const revealIds = useConversationMessageRevealIds({
    isActive: true,
    liveTurnVisible: false,
    messages: makeMessages(messageIds),
    pendingPromptIds: [],
    sessionId,
    userScrollGeneration,
  });
  const tailMessageId = messageIds[messageIds.length - 1] ?? null;
  return mountTail && tailMessageId
    ? createElement(RevealMount, {
        messageId: tailMessageId,
        revealInCurrentCommit: revealIds.has(tailMessageId),
        revealUserScrollGeneration: userScrollGeneration,
        sessionId,
        userScrollGeneration,
      })
    : null;
}

describe("conversation message reveal mount handoff", () => {
  it("keeps an appended id pending until a later virtualized mount consumes it", () => {
    const sessionId = "session-delayed-virtual-mount";
    const { queryByTestId, rerender } = render(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1"],
        mountTail: false,
        sessionId,
        userScrollGeneration: 0,
      }),
    );

    rerender(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1", "message-2"],
        mountTail: false,
        sessionId,
        userScrollGeneration: 0,
      }),
    );
    expect(queryByTestId("reveal-probe")).toBeNull();

    rerender(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1", "message-2"],
        mountTail: true,
        sessionId,
        userScrollGeneration: 0,
      }),
    );
    expect(queryByTestId("reveal-probe")).toHaveAttribute(
      "data-will-reveal",
      "true",
    );

    rerender(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1", "message-2"],
        mountTail: false,
        sessionId,
        userScrollGeneration: 0,
      }),
    );
    rerender(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1", "message-2"],
        mountTail: true,
        sessionId,
        userScrollGeneration: 0,
      }),
    );
    expect(queryByTestId("reveal-probe")).toHaveAttribute(
      "data-will-reveal",
      "false",
    );
  });

  it("prunes a pending id when user navigation advances before mount", () => {
    const sessionId = "session-stale-virtual-mount";
    const { queryByTestId, rerender } = render(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1"],
        mountTail: false,
        sessionId,
        userScrollGeneration: 0,
      }),
    );

    rerender(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1", "message-2"],
        mountTail: false,
        sessionId,
        userScrollGeneration: 0,
      }),
    );
    rerender(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1", "message-2"],
        mountTail: true,
        sessionId,
        userScrollGeneration: 1,
      }),
    );

    expect(queryByTestId("reveal-probe")).toHaveAttribute(
      "data-will-reveal",
      "false",
    );
  });
});
