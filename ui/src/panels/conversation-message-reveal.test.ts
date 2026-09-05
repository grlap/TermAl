import { render } from "@testing-library/react";
import { createElement } from "react";
import { afterEach, describe, expect, it } from "vitest";

import {
  CONVERSATION_MESSAGE_ENTRY_REVEAL_CLASS_NAME,
  cancelConversationMessageEntryReveals,
  getConversationMessageRevealRegistrySizeForTesting,
  resetConversationMessageRevealRegistryForTesting,
  resolveConversationMessageRevealTransition,
  useConversationMessageRevealIds,
  useConversationMessageRevealOnMount,
  type ConversationMessageRevealInput,
  type ConversationMessageRevealState,
} from "./conversation-message-reveal";
import type { Message } from "../types";

afterEach(() => {
  resetConversationMessageRevealRegistryForTesting();
});

const input = (
  messageIds: readonly string[],
  overrides: Partial<ConversationMessageRevealInput> = {},
): ConversationMessageRevealInput => ({
  isActive: true,
  isTurnActive: false,
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
  it("cancels active reveal classes without touching unrelated message shells", () => {
    const root = document.createElement("section");
    const revealing = document.createElement("div");
    const settled = document.createElement("div");
    revealing.classList.add(CONVERSATION_MESSAGE_ENTRY_REVEAL_CLASS_NAME);
    root.append(revealing, settled);

    cancelConversationMessageEntryReveals(root);

    expect(revealing).not.toHaveClass(
      CONVERSATION_MESSAGE_ENTRY_REVEAL_CLASS_NAME,
    );
    expect(revealing).toHaveAttribute(
      "data-conversation-message-entry-reveal-cancelled",
    );
    expect(settled).not.toHaveAttribute(
      "data-conversation-message-entry-reveal-cancelled",
    );
  });

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

  it("does not reveal a final message at explicit turn completion", () => {
    const live = advance(
      null,
      input(["message-1"], { isTurnActive: true }),
    );
    const completed = advance(
      live.nextState,
      input(["message-1", "message-final"], { isTurnActive: false }),
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
  revealScopeKey,
  revealInCurrentCommit,
  revealUserScrollGeneration,
  userScrollGeneration,
}: {
  messageId: string;
  revealScopeKey: string;
  revealInCurrentCommit: boolean;
  revealUserScrollGeneration: number;
  userScrollGeneration: number;
}) {
  const shouldReveal = useConversationMessageRevealOnMount({
    messageId,
    revealScopeKey,
    revealInCurrentCommit,
    revealUserScrollGeneration,
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
  revealScopeKey,
  userScrollGeneration,
}: {
  messageIds: readonly string[];
  mountTail: boolean;
  revealScopeKey: string;
  userScrollGeneration: number;
}) {
  const revealIds = useConversationMessageRevealIds({
    isActive: true,
    isTurnActive: false,
    messages: makeMessages(messageIds),
    pendingPromptIds: [],
    revealScopeKey,
    userScrollGeneration,
  });
  const tailMessageId = messageIds[messageIds.length - 1] ?? null;
  return mountTail && tailMessageId
    ? createElement(RevealMount, {
        messageId: tailMessageId,
        revealScopeKey,
        revealInCurrentCommit: revealIds.has(tailMessageId),
        revealUserScrollGeneration: userScrollGeneration,
        userScrollGeneration,
      })
    : null;
}

function RevealScopeProbe({
  messageIds,
  revealScopeKey,
  testId,
}: {
  messageIds: readonly string[];
  revealScopeKey: string;
  testId: string;
}) {
  const revealIds = useConversationMessageRevealIds({
    isActive: true,
    isTurnActive: false,
    messages: makeMessages(messageIds),
    pendingPromptIds: [],
    revealScopeKey,
    userScrollGeneration: 0,
  });
  return createElement("div", {
    "data-reveal-ids": [...revealIds].join(","),
    "data-testid": testId,
  });
}

function DuplicateSessionScopesHarness({
  newerMessageIds,
  olderMessageIds,
}: {
  newerMessageIds: readonly string[];
  olderMessageIds: readonly string[];
}) {
  return createElement(
    "div",
    null,
    createElement(RevealScopeProbe, {
      messageIds: newerMessageIds,
      revealScopeKey: "pane-newer:session-shared",
      testId: "newer-scope",
    }),
    createElement(RevealScopeProbe, {
      messageIds: olderMessageIds,
      revealScopeKey: "pane-older:session-shared",
      testId: "older-scope",
    }),
  );
}

describe("conversation message reveal mount handoff", () => {
  it("keeps disjoint panes of the same session on independent watermarks", () => {
    const { getByTestId, rerender } = render(
      createElement(DuplicateSessionScopesHarness, {
        newerMessageIds: ["message-100", "message-101"],
        olderMessageIds: ["message-1", "message-2"],
      }),
    );

    rerender(
      createElement(DuplicateSessionScopesHarness, {
        newerMessageIds: ["message-100", "message-101", "message-102"],
        olderMessageIds: ["message-1", "message-2"],
      }),
    );

    expect(getByTestId("newer-scope")).toHaveAttribute(
      "data-reveal-ids",
      "message-102",
    );
    expect(getByTestId("older-scope")).toHaveAttribute(
      "data-reveal-ids",
      "",
    );
  });

  it("bounds retained reveal scopes while preserving recent continuity", () => {
    const { rerender } = render(
      createElement(RevealScopeProbe, {
        messageIds: ["message-1"],
        revealScopeKey: "pane-0:session-0",
        testId: "bounded-scope",
      }),
    );

    for (let index = 1; index <= 300; index += 1) {
      rerender(
        createElement(RevealScopeProbe, {
          messageIds: ["message-1"],
          revealScopeKey: `pane-${index}:session-${index}`,
          testId: "bounded-scope",
        }),
      );
    }

    expect(getConversationMessageRevealRegistrySizeForTesting()).toBe(256);
  });

  it("keeps an appended id pending until a later virtualized mount consumes it", () => {
    const revealScopeKey = "pane-1:session-delayed-virtual-mount";
    const { queryByTestId, rerender } = render(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1"],
        mountTail: false,
        revealScopeKey,
        userScrollGeneration: 0,
      }),
    );

    rerender(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1", "message-2"],
        mountTail: false,
        revealScopeKey,
        userScrollGeneration: 0,
      }),
    );
    expect(queryByTestId("reveal-probe")).toBeNull();

    rerender(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1", "message-2"],
        mountTail: true,
        revealScopeKey,
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
        revealScopeKey,
        userScrollGeneration: 0,
      }),
    );
    rerender(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1", "message-2"],
        mountTail: true,
        revealScopeKey,
        userScrollGeneration: 0,
      }),
    );
    expect(queryByTestId("reveal-probe")).toHaveAttribute(
      "data-will-reveal",
      "false",
    );
  });

  it("prunes a pending id when user navigation advances before mount", () => {
    const revealScopeKey = "pane-1:session-stale-virtual-mount";
    const { queryByTestId, rerender } = render(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1"],
        mountTail: false,
        revealScopeKey,
        userScrollGeneration: 0,
      }),
    );

    rerender(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1", "message-2"],
        mountTail: false,
        revealScopeKey,
        userScrollGeneration: 0,
      }),
    );
    rerender(
      createElement(DelayedMountHarness, {
        messageIds: ["message-1", "message-2"],
        mountTail: true,
        revealScopeKey,
        userScrollGeneration: 1,
      }),
    );

    expect(queryByTestId("reveal-probe")).toHaveAttribute(
      "data-will-reveal",
      "false",
    );
  });
});
