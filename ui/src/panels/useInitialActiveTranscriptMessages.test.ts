// Owns focused tests for initial active transcript tail-window helpers.
// Does not own AgentSessionPanel rendering, virtualization, or composer tests.
// Split from ui/src/panels/AgentSessionPanel.test.tsx.

import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import {
  includeUndeferredMessageTail,
  useInitialActiveTranscriptMessages,
} from "./useInitialActiveTranscriptMessages";
import { SESSION_TAIL_WINDOW_MESSAGE_COUNT } from "../session-tail-policy";
import type { Message } from "../types";
import type { RefObject } from "react";

function makeTextMessages(count: number): Message[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `message-${index + 1}`,
    type: "text",
    timestamp: `10:${String(index).padStart(2, "0")}`,
    author: index % 2 === 0 ? "you" : "assistant",
    text: `Message ${index + 1}`,
  }));
}

function makeScrollNodeRef() {
  const node = document.createElement("div");
  document.body.append(node);
  return {
    cleanup: () => node.remove(),
    node,
    ref: { current: node } as RefObject<HTMLElement | null>,
  };
}

function renderInitialActiveTranscriptHook({
  hasConversationMarkers = false,
  hasConversationSearch = false,
  isActive = true,
  messages = makeTextMessages(600),
  scrollContainerRef = {
    current: document.createElement("div"),
  } as RefObject<HTMLElement | null>,
  sessionId = "session-a",
}: {
  hasConversationMarkers?: boolean;
  hasConversationSearch?: boolean;
  isActive?: boolean;
  messages?: Message[];
  scrollContainerRef?: RefObject<HTMLElement | null>;
  sessionId?: string;
} = {}) {
  return renderHook((props) => useInitialActiveTranscriptMessages(props), {
    initialProps: {
      hasConversationMarkers,
      hasConversationSearch,
      isActive,
      messages,
      scrollContainerRef,
      sessionId,
    },
  });
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

describe("useInitialActiveTranscriptMessages", () => {
  it("starts active long transcripts in a tail window", () => {
    const messages = makeTextMessages(600);
    const hook = renderInitialActiveTranscriptHook({ messages });

    expect(hook.result.current.isWindowed).toBe(true);
    expect(hook.result.current.messages).toEqual(
      messages.slice(-SESSION_TAIL_WINDOW_MESSAGE_COUNT),
    );
  });

  it("renders the full transcript after an explicit demand request", () => {
    const messages = makeTextMessages(600);
    const hook = renderInitialActiveTranscriptHook({ messages });

    act(() => {
      expect(hook.result.current.requestFullTranscriptRender()).toBe(true);
    });

    expect(hook.result.current.isWindowed).toBe(false);
    expect(hook.result.current.messages).toBe(messages);
    expect(hook.result.current.requestFullTranscriptRender()).toBe(false);
  });

  it("keeps long transcripts full when markers or search need the whole transcript", () => {
    const messages = makeTextMessages(600);

    const markerHook = renderInitialActiveTranscriptHook({
      hasConversationMarkers: true,
      messages,
    });
    expect(markerHook.result.current.isWindowed).toBe(false);
    expect(markerHook.result.current.messages).toBe(messages);

    const searchHook = renderInitialActiveTranscriptHook({
      hasConversationSearch: true,
      messages,
    });
    expect(searchHook.result.current.isWindowed).toBe(false);
    expect(searchHook.result.current.messages).toBe(messages);
  });

  it("resets explicit hydration state when the session id changes", () => {
    const messages = makeTextMessages(600);
    const hook = renderInitialActiveTranscriptHook({ messages });

    act(() => {
      hook.result.current.requestFullTranscriptRender();
    });
    expect(hook.result.current.isWindowed).toBe(false);

    hook.rerender({
      hasConversationMarkers: false,
      hasConversationSearch: false,
      isActive: true,
      messages,
      scrollContainerRef: { current: document.createElement("div") },
      sessionId: "session-b",
    });

    expect(hook.result.current.isWindowed).toBe(true);
  });

  it("hydrates after top-boundary transcript scroll demand", () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    const hook = renderInitialActiveTranscriptHook({ scrollContainerRef: ref });

    act(() => {
      node.scrollTop = 200;
      node.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
      node.scrollTop = 0;
      node.dispatchEvent(new Event("scroll", { bubbles: true }));
    });

    expect(hook.result.current.isWindowed).toBe(false);
    cleanup();
  });

  it("hydrates after upward wheel demand near the transcript top", async () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    const hook = renderInitialActiveTranscriptHook({ scrollContainerRef: ref });

    await act(async () => {
      node.scrollTop = 0;
      node.dispatchEvent(
        new WheelEvent("wheel", { bubbles: true, deltaY: -20 }),
      );
      await Promise.resolve();
    });

    expect(hook.result.current.isWindowed).toBe(false);
    cleanup();
  });

  it("hydrates from transcript keyboard demand but ignores typing targets", () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    const input = document.createElement("input");
    node.append(input);
    const hook = renderInitialActiveTranscriptHook({ scrollContainerRef: ref });

    act(() => {
      input.dispatchEvent(
        new KeyboardEvent("keydown", { bubbles: true, key: "PageUp" }),
      );
    });
    expect(hook.result.current.isWindowed).toBe(true);

    act(() => {
      node.dispatchEvent(
        new KeyboardEvent("keydown", { bubbles: true, key: "PageUp" }),
      );
    });
    expect(hook.result.current.isWindowed).toBe(false);
    cleanup();
  });

  it("hydrates after a pull-down touch gesture near the transcript top", () => {
    const { cleanup, node, ref } = makeScrollNodeRef();
    const hook = renderInitialActiveTranscriptHook({ scrollContainerRef: ref });
    const startEvent = new Event("touchstart", { bubbles: true });
    Object.defineProperty(startEvent, "touches", {
      value: [{ clientY: 10 }],
    });
    const moveEvent = new Event("touchmove", { bubbles: true });
    Object.defineProperty(moveEvent, "touches", {
      value: [{ clientY: 30 }],
    });
    Object.defineProperty(moveEvent, "changedTouches", {
      value: [],
    });

    act(() => {
      node.scrollTop = 0;
      node.dispatchEvent(startEvent);
      node.dispatchEvent(moveEvent);
    });

    expect(hook.result.current.isWindowed).toBe(false);
    cleanup();
  });
});
