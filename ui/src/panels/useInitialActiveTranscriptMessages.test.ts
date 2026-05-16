// Owns focused tests for initial active transcript tail-window helpers.
// Does not own AgentSessionPanel rendering, virtualization, or composer tests.
// Split from ui/src/panels/AgentSessionPanel.test.tsx.

import { describe, expect, it } from "vitest";

import { includeUndeferredMessageTail } from "./useInitialActiveTranscriptMessages";
import type { Message } from "../types";

function makeTextMessages(count: number): Message[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `message-${index + 1}`,
    type: "text",
    timestamp: `10:${String(index).padStart(2, "0")}`,
    author: index % 2 === 0 ? "you" : "assistant",
    text: `Message ${index + 1}`,
  }));
}

describe("includeUndeferredMessageTail", () => {
  it("renders current same-id message updates at the active transcript tail", () => {
    const stableMessage: Message = {
      author: "you",
      id: "message-1",
      text: "Prompt",
      timestamp: "10:00",
      type: "text",
    };
    const deferredAssistant: Message = {
      author: "assistant",
      id: "message-2",
      text: "Old streamed answer",
      timestamp: "10:01",
      type: "text",
    };
    const currentAssistant: Message = {
      ...deferredAssistant,
      text: "Old streamed answer plus the latest chunk",
    };

    expect(
      includeUndeferredMessageTail(
        [stableMessage, deferredAssistant],
        [stableMessage, currentAssistant],
      ),
    ).toEqual([stableMessage, currentAssistant]);
  });

  it("drops deferred transcript objects when the active session changes", () => {
    const deferredMessage: Message = {
      author: "assistant",
      id: "message-a",
      text: "Session A",
      timestamp: "10:00",
      type: "text",
    };
    const currentMessage: Message = {
      author: "assistant",
      id: "message-b",
      text: "Session B",
      timestamp: "10:00",
      type: "text",
    };

    const currentMessages = [currentMessage];

    expect(includeUndeferredMessageTail([deferredMessage], currentMessages)).toBe(
      currentMessages,
    );
  });

  it("drops deferred transcript objects when the current session is empty", () => {
    const deferredMessage: Message = {
      author: "assistant",
      id: "message-a",
      text: "Previous session",
      timestamp: "10:00",
      type: "text",
    };
    const currentMessages: Message[] = [];

    expect(includeUndeferredMessageTail([deferredMessage], currentMessages)).toBe(
      currentMessages,
    );
  });

  it("keeps the deferred array when all current messages are unchanged", () => {
    const message = makeTextMessages(1)[0];
    const deferredMessages = [message];

    expect(
      includeUndeferredMessageTail(deferredMessages, deferredMessages),
    ).toBe(deferredMessages);
  });

  it("returns the current array when deferred messages include pruned tail items", () => {
    const currentMessages = makeTextMessages(1);
    const deferredMessages = [...currentMessages, makeTextMessages(2)[1]];

    expect(includeUndeferredMessageTail(deferredMessages, currentMessages)).toBe(
      currentMessages,
    );
  });

  it("splices from the first same-id message object change and preserves stable deferred prefix", () => {
    const [firstMessage, deferredChangedMessage, trailingMessage] =
      makeTextMessages(3);
    const currentChangedMessage: Message = {
      author: "assistant",
      id: deferredChangedMessage.id,
      text: "Updated middle message",
      timestamp: deferredChangedMessage.timestamp,
      type: "text",
    };
    const result = includeUndeferredMessageTail(
      [firstMessage, deferredChangedMessage, trailingMessage],
      [firstMessage, currentChangedMessage, trailingMessage],
    );

    expect(result).toEqual([
      firstMessage,
      currentChangedMessage,
      trailingMessage,
    ]);
    expect(result[0]).toBe(firstMessage);
    expect(result[1]).toBe(currentChangedMessage);
    expect(result[2]).toBe(trailingMessage);
  });

  it("appends current tail messages after an unchanged deferred prefix", () => {
    const [firstMessage, secondMessage] = makeTextMessages(2);
    const deferredMessages = [firstMessage];
    const result = includeUndeferredMessageTail(deferredMessages, [
      firstMessage,
      secondMessage,
    ]);

    expect(result).toEqual([firstMessage, secondMessage]);
    expect(result[0]).toBe(firstMessage);
    expect(result[1]).toBe(secondMessage);
  });
});
