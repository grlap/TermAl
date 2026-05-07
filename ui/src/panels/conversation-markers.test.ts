import { describe, expect, it, vi } from "vitest";
import { act, render } from "@testing-library/react";
import { createElement, useEffect, useRef } from "react";

import {
  ConversationMarkerNavigator,
  findActivatableConversationMarkerContextMenuTrigger,
  findMountedConversationMessageSlot,
  groupConversationMarkersByMessageId,
  sortConversationMarkersForNavigation,
  useConversationMarkerJump,
} from "./conversation-markers";
import { DEFAULT_CONVERSATION_MARKER_COLOR } from "../conversation-marker-colors";
import type { ConversationMarker, Message } from "../types";
import type { VirtualizedConversationMessageListHandle } from "./VirtualizedConversationMessageList";

function makeMessage(id: string): Message {
  return {
    id,
    timestamp: "2026-05-02 10:00:00",
    author: "assistant",
    type: "text",
    text: `Message ${id}`,
  };
}

function makeMarker(
  id: string,
  overrides: Partial<ConversationMarker> = {},
): ConversationMarker {
  return {
    id,
    sessionId: "session-1",
    kind: "custom",
    name: `Marker ${id}`,
    body: null,
    color: "#8ab4f8",
    messageId: "message-1",
    messageIndexHint: 0,
    endMessageId: null,
    endMessageIndexHint: null,
    createdAt: "2026-05-02 10:00:00",
    updatedAt: "2026-05-02 10:00:00",
    createdBy: "user",
    ...overrides,
  };
}

type MarkerJumpApi = ReturnType<typeof useConversationMarkerJump>;

function makeVirtualizerHandle(
  jumpToMessageId = vi.fn(() => true),
): VirtualizedConversationMessageListHandle {
  return {
    getLayoutSnapshot: vi.fn(),
    getViewportSnapshot: vi.fn(),
    jumpToMessageId,
    jumpToMessageIndex: vi.fn(() => false),
  } as unknown as VirtualizedConversationMessageListHandle;
}

function MarkerJumpHarness({
  onReady,
  scrollRoot,
  sessionId,
  virtualizerHandle,
}: {
  onReady: (api: MarkerJumpApi) => void;
  scrollRoot: HTMLElement;
  sessionId: string;
  virtualizerHandle: VirtualizedConversationMessageListHandle;
}) {
  const scrollContainerRef = useRef<HTMLElement | null>(scrollRoot);
  const virtualizerHandleRef =
    useRef<VirtualizedConversationMessageListHandle | null>(virtualizerHandle);
  scrollContainerRef.current = scrollRoot;
  virtualizerHandleRef.current = virtualizerHandle;
  const api = useConversationMarkerJump({
    onConversationSearchItemMount: () => {},
    scrollContainerRef,
    sessionId,
    virtualizerHandleRef,
  });

  useEffect(() => {
    onReady(api);
  }, [api, onReady]);

  return null;
}

function installManualAnimationFrames({
  cancelRemovesCallbacks = true,
}: {
  cancelRemovesCallbacks?: boolean;
} = {}) {
  const originalRequestAnimationFrame = window.requestAnimationFrame;
  const originalCancelAnimationFrame = window.cancelAnimationFrame;
  const callbacks = new Map<number, FrameRequestCallback>();
  let nextFrameId = 1;

  window.requestAnimationFrame = vi.fn((callback: FrameRequestCallback) => {
    const frameId = nextFrameId;
    nextFrameId += 1;
    callbacks.set(frameId, callback);
    return frameId;
  }) as typeof window.requestAnimationFrame;
  window.cancelAnimationFrame = vi.fn((frameId: number) => {
    if (cancelRemovesCallbacks) {
      callbacks.delete(frameId);
    }
  }) as typeof window.cancelAnimationFrame;

  return {
    callbacks,
    flushNextFrame() {
      const next = callbacks.entries().next();
      if (next.done) {
        throw new Error("Expected a queued animation frame");
      }
      const [frameId, callback] = next.value;
      callbacks.delete(frameId);
      callback(0);
    },
    restore() {
      window.requestAnimationFrame = originalRequestAnimationFrame;
      window.cancelAnimationFrame = originalCancelAnimationFrame;
    },
  };
}

describe("conversation marker helpers", () => {
  it("groups markers by message id without reordering markers inside a group", () => {
    const first = makeMarker("marker-1", { messageId: "message-1" });
    const second = makeMarker("marker-2", { messageId: "message-2" });
    const third = makeMarker("marker-3", { messageId: "message-1" });

    const grouped = groupConversationMarkersByMessageId([first, second, third]);

    expect(grouped.get("message-1")).toEqual([first, third]);
    expect(grouped.get("message-2")).toEqual([second]);
  });

  it("finds mounted message slots by search item key within the supplied root", () => {
    const root = document.createElement("section");
    const outside = document.createElement("div");
    const matching = document.createElement("article");
    const unrelated = document.createElement("article");

    outside.dataset.sessionSearchItemKey = "message:message-1";
    matching.dataset.sessionSearchItemKey = "message:message-1";
    unrelated.dataset.sessionSearchItemKey = "message:message-2";
    root.append(unrelated, matching);
    document.body.append(outside);

    try {
      expect(findMountedConversationMessageSlot("message-1", root)).toBe(
        matching,
      );
      expect(findMountedConversationMessageSlot("message-2", root)).toBe(
        unrelated,
      );
      expect(findMountedConversationMessageSlot("missing", root)).toBeNull();
    } finally {
      outside.remove();
    }
  });

  it("uses the document as the default mounted-slot lookup root", () => {
    const matching = document.createElement("article");
    const unrelated = document.createElement("article");
    matching.dataset.sessionSearchItemKey = "message:message-1";
    unrelated.dataset.sessionSearchItemKey = "message:message-2";
    document.body.append(unrelated, matching);

    try {
      expect(findMountedConversationMessageSlot("message-1")).toBe(matching);
      expect(findMountedConversationMessageSlot("message-2")).toBe(unrelated);
      expect(findMountedConversationMessageSlot("missing")).toBeNull();
    } finally {
      matching.remove();
      unrelated.remove();
    }
  });

  it("rejects nested native controls inside custom marker-menu triggers", () => {
    const root = document.createElement("section");
    const trigger = document.createElement("span");
    const nestedButton = document.createElement("button");
    trigger.dataset.conversationMarkerMenuTrigger = "true";
    trigger.append(nestedButton);
    root.append(trigger);

    expect(
      findActivatableConversationMarkerContextMenuTrigger(root, trigger),
    ).toBe(trigger);
    expect(
      findActivatableConversationMarkerContextMenuTrigger(root, nestedButton),
    ).toBeNull();
  });

  it("sorts markers by mounted message order before persisted index hints", () => {
    const messages = [makeMessage("message-1"), makeMessage("message-2")];
    const later = makeMarker("marker-1", {
      messageId: "message-2",
      messageIndexHint: 0,
    });
    const earlier = makeMarker("marker-2", {
      messageId: "message-1",
      messageIndexHint: 99,
    });

    expect(
      sortConversationMarkersForNavigation([later, earlier], messages),
    ).toEqual([earlier, later]);
  });

  it("falls back to index hints, creation time, and id for stable navigation", () => {
    const newest = makeMarker("marker-c", {
      messageId: "missing-1",
      messageIndexHint: 5,
      createdAt: "2026-05-02 10:00:02",
    });
    const oldestById = makeMarker("marker-a", {
      messageId: "missing-2",
      messageIndexHint: 5,
      createdAt: "2026-05-02 10:00:01",
    });
    const oldestByIdAfter = makeMarker("marker-b", {
      messageId: "missing-3",
      messageIndexHint: 5,
      createdAt: "2026-05-02 10:00:01",
    });
    const earliestHint = makeMarker("marker-d", {
      messageId: "missing-4",
      messageIndexHint: 1,
      createdAt: "2026-05-02 10:00:03",
    });

    expect(
      sortConversationMarkersForNavigation(
        [newest, oldestByIdAfter, earliestHint, oldestById],
        [],
      ),
    ).toEqual([earliestHint, oldestById, oldestByIdAfter, newest]);
  });

  it("recovers a marker jump after the virtualized target mounts on the second frame", () => {
    const frames = installManualAnimationFrames();
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    const scrollIntoView = vi.fn();
    const scrollRoot = document.createElement("section");
    const virtualizerHandle = makeVirtualizerHandle();
    let markerJump: MarkerJumpApi | null = null;
    HTMLElement.prototype.scrollIntoView = function scrollIntoViewMock(
      this: HTMLElement,
      options?: ScrollIntoViewOptions,
    ) {
      scrollIntoView(this.dataset.sessionSearchItemKey, options);
    };

    try {
      render(
        createElement(MarkerJumpHarness, {
          onReady: (api) => {
            markerJump = api;
          },
          scrollRoot,
          sessionId: "session-1",
          virtualizerHandle,
        }),
      );

      act(() => {
        markerJump?.jumpToMarker(
          makeMarker("marker-1", { messageId: "message-1" }),
        );
      });

      expect(virtualizerHandle.jumpToMessageId).toHaveBeenCalledWith(
        "message-1",
        { align: "center", flush: true },
      );
      expect(scrollIntoView).not.toHaveBeenCalled();
      expect(frames.callbacks.size).toBe(1);

      act(() => {
        frames.flushNextFrame();
      });
      expect(scrollIntoView).not.toHaveBeenCalled();
      expect(frames.callbacks.size).toBe(1);

      const markerSlot = document.createElement("article");
      markerSlot.dataset.sessionSearchItemKey = "message:message-1";
      scrollRoot.append(markerSlot);
      act(() => {
        frames.flushNextFrame();
      });

      expect(scrollIntoView).toHaveBeenCalledWith("message:message-1", {
        block: "center",
        behavior: "auto",
      });
      expect(frames.callbacks.size).toBe(0);
    } finally {
      frames.restore();
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
    }
  });

  it("ignores a delayed marker-jump correction after the session changes", () => {
    const frames = installManualAnimationFrames({
      cancelRemovesCallbacks: false,
    });
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    const scrollIntoView = vi.fn();
    const scrollRoot = document.createElement("section");
    const virtualizerHandle = makeVirtualizerHandle();
    let markerJump: MarkerJumpApi | null = null;
    HTMLElement.prototype.scrollIntoView = function scrollIntoViewMock(
      this: HTMLElement,
      options?: ScrollIntoViewOptions,
    ) {
      scrollIntoView(this.dataset.sessionSearchItemKey, options);
    };

    try {
      const { rerender } = render(
        createElement(MarkerJumpHarness, {
          onReady: (api) => {
            markerJump = api;
          },
          scrollRoot,
          sessionId: "session-1",
          virtualizerHandle,
        }),
      );

      act(() => {
        markerJump?.jumpToMarker(
          makeMarker("marker-1", { messageId: "shared-message" }),
        );
      });
      expect(frames.callbacks.size).toBe(1);

      rerender(
        createElement(MarkerJumpHarness, {
          onReady: (api) => {
            markerJump = api;
          },
          scrollRoot,
          sessionId: "session-2",
          virtualizerHandle,
        }),
      );
      const newSessionSlot = document.createElement("article");
      newSessionSlot.dataset.sessionSearchItemKey = "message:shared-message";
      scrollRoot.append(newSessionSlot);

      act(() => {
        frames.flushNextFrame();
      });

      expect(scrollIntoView).not.toHaveBeenCalled();
    } finally {
      frames.restore();
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
    }
  });

  it("falls back to mounted slot lookup when the virtualizer cannot jump", () => {
    const frames = installManualAnimationFrames();
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    const scrollIntoView = vi.fn();
    const scrollRoot = document.createElement("section");
    const markerSlot = document.createElement("article");
    const virtualizerHandle = makeVirtualizerHandle(vi.fn(() => false));
    let markerJump: MarkerJumpApi | null = null;
    markerSlot.dataset.sessionSearchItemKey = "message:message-1";
    scrollRoot.append(markerSlot);
    HTMLElement.prototype.scrollIntoView = function scrollIntoViewMock(
      this: HTMLElement,
      options?: ScrollIntoViewOptions,
    ) {
      scrollIntoView(this.dataset.sessionSearchItemKey, options);
    };

    try {
      render(
        createElement(MarkerJumpHarness, {
          onReady: (api) => {
            markerJump = api;
          },
          scrollRoot,
          sessionId: "session-1",
          virtualizerHandle,
        }),
      );

      act(() => {
        markerJump?.jumpToMarker(
          makeMarker("marker-1", { messageId: "message-1" }),
        );
      });

      expect(virtualizerHandle.jumpToMessageId).toHaveBeenCalledWith(
        "message-1",
        { align: "center", flush: true },
      );
      expect(scrollIntoView).toHaveBeenCalledTimes(1);
      expect(scrollIntoView).toHaveBeenCalledWith("message:message-1", {
        block: "center",
        behavior: "smooth",
      });
      expect(frames.callbacks.size).toBe(0);
    } finally {
      frames.restore();
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
    }
  });

  it("does not schedule marker jump correction when the mounted slot is found synchronously", () => {
    const frames = installManualAnimationFrames();
    const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;
    const scrollIntoView = vi.fn();
    const scrollRoot = document.createElement("section");
    const markerSlot = document.createElement("article");
    const virtualizerHandle = makeVirtualizerHandle();
    let markerJump: MarkerJumpApi | null = null;
    markerSlot.dataset.sessionSearchItemKey = "message:message-1";
    scrollRoot.append(markerSlot);
    HTMLElement.prototype.scrollIntoView = function scrollIntoViewMock(
      this: HTMLElement,
      options?: ScrollIntoViewOptions,
    ) {
      scrollIntoView(this.dataset.sessionSearchItemKey, options);
    };

    try {
      render(
        createElement(MarkerJumpHarness, {
          onReady: (api) => {
            markerJump = api;
          },
          scrollRoot,
          sessionId: "session-1",
          virtualizerHandle,
        }),
      );

      act(() => {
        markerJump?.jumpToMarker(
          makeMarker("marker-1", { messageId: "message-1" }),
        );
      });

      expect(virtualizerHandle.jumpToMessageId).toHaveBeenCalledWith(
        "message-1",
        { align: "center", flush: true },
      );
      expect(scrollIntoView).toHaveBeenCalledTimes(1);
      expect(scrollIntoView).toHaveBeenCalledWith("message:message-1", {
        block: "center",
        behavior: "auto",
      });
      expect(frames.callbacks.size).toBe(0);
    } finally {
      frames.restore();
      HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
    }
  });

  it.each([
    "url(https://example.test/marker)",
    "var(--signal-blue)",
    "linear-gradient(red, blue)",
  ])("normalizes chip color %s before assigning CSS custom properties", (color) => {
    const marker = makeMarker("marker-1", {
      color,
    });
    const { container } = render(
      createElement(ConversationMarkerNavigator, {
        markers: [marker],
        activeMarkerId: null,
        onJump: () => {},
        onNavigatePrevious: () => {},
        onNavigateNext: () => {},
      }),
    );

    const chip = container.querySelector<HTMLElement>(
      ".conversation-marker-chip",
    );

    expect(chip).not.toBeNull();
    expect(chip?.style.getPropertyValue("--conversation-marker-color")).toBe(
      DEFAULT_CONVERSATION_MARKER_COLOR,
    );
  });
});
