import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ConversationOverviewRail } from "./ConversationOverviewRail";
import { CONVERSATION_COMPOSER_INPUT_DATASET_KEY } from "./conversation-composer-focus";
import type { VirtualizedConversationLayoutSnapshot } from "./VirtualizedConversationMessageList";
import { DEFAULT_CONVERSATION_MARKER_COLOR } from "../conversation-marker-colors";
import type { Message } from "../types";

function textMessages(count: number): Message[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `message-${index + 1}`,
    type: "text",
    author: index % 2 === 0 ? "you" : "assistant",
    timestamp: `10:${String(index).padStart(2, "0")}`,
    text: `Message ${index + 1}`,
  }));
}

function assistantTextMessages(count: number): Message[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `assistant-message-${index + 1}`,
    type: "text",
    author: "assistant",
    timestamp: `11:${String(index).padStart(2, "0")}`,
    text: `Assistant message ${index + 1}`,
  }));
}

function commandMessages(count: number): Message[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `command-message-${index + 1}`,
    type: "command",
    author: "assistant",
    timestamp: `12:${String(index).padStart(2, "0")}`,
    command: "npm test",
    output: "ok",
    status: index % 2 === 0 ? "error" : "success",
  }));
}

function layoutSnapshot(messages: Message[]): VirtualizedConversationLayoutSnapshot {
  return {
    sessionId: "session-1",
    messageCount: messages.length,
    estimatedTotalHeightPx: messages.length * 100,
    viewportTopPx: 200,
    viewportHeightPx: 300,
    viewportWidthPx: 800,
    isActive: true,
    visiblePageRange: {
      startIndex: 2,
      endIndex: 5,
    },
    mountedPageRange: {
      startIndex: 0,
      endIndex: 7,
    },
    messages: messages.map((message, index) => ({
      messageId: message.id,
      messageIndex: index,
      pageIndex: Math.floor(index / 8),
      type: message.type,
      author: message.author,
      estimatedTopPx: index * 100,
      estimatedHeightPx: 100,
      measuredPageHeightPx: null,
    })),
  };
}

describe("ConversationOverviewRail", () => {
  it("does not render below the long-session threshold", () => {
    render(
      <ConversationOverviewRail
        messages={textMessages(3)}
        layoutSnapshot={null}
        minMessages={4}
        onNavigate={() => {}}
      />,
    );

    expect(
      screen.queryByLabelText("Conversation overview"),
    ).not.toBeInTheDocument();
  });

  it("renders overview items, viewport, and marker pins", () => {
    const messages = textMessages(5);

    render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={layoutSnapshot(messages)}
        markers={[
          {
            id: "marker-1",
            messageId: "message-3",
            name: "Important section",
            color: "#38bdf8",
          },
        ]}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={() => {}}
      />,
    );

    expect(screen.getByLabelText("Conversation overview")).toBeInTheDocument();
    expect(screen.getByLabelText(/User prompt 1/)).toBeInTheDocument();
    expect(screen.getByLabelText("Important section")).toBeInTheDocument();
    expect(
      screen
        .getByLabelText("Important section")
        .style.getPropertyValue("--conversation-overview-marker-color"),
    ).toBe("#38bdf8");
    expect(screen.getByTestId("conversation-overview-viewport")).toHaveStyle({
      top: "100px",
      height: "150px",
    });
  });

  it("keeps the previous projection while the composer prompt is focused", () => {
    vi.useFakeTimers();
    const composer = document.createElement("textarea");
    composer.dataset[CONVERSATION_COMPOSER_INPUT_DATASET_KEY] = "true";
    document.body.appendChild(composer);

    try {
      const messages = textMessages(5);
      const updatedMessages = messages.map((message, index) =>
        index === 0 && message.type === "text"
          ? { ...message, text: "Updated prompt sample" }
          : message,
      );
      act(() => {
        composer.focus();
        fireEvent.focusIn(composer);
      });

      const { rerender } = render(
        <ConversationOverviewRail
          messages={messages}
          layoutSnapshot={layoutSnapshot(messages)}
          minMessages={4}
          maxHeightPx={250}
          onNavigate={() => {}}
        />,
      );

      expect(screen.getByLabelText(/User prompt 1: Message 1/)).toBeInTheDocument();

      rerender(
        <ConversationOverviewRail
          messages={updatedMessages}
          layoutSnapshot={layoutSnapshot(updatedMessages)}
          minMessages={4}
          maxHeightPx={250}
          onNavigate={() => {}}
        />,
      );

      expect(screen.getByLabelText(/User prompt 1: Message 1/)).toBeInTheDocument();
      expect(
        screen.queryByLabelText(/User prompt 1: Updated prompt sample/),
      ).not.toBeInTheDocument();

      act(() => {
        composer.blur();
        fireEvent.focusOut(composer);
      });
      rerender(
        <ConversationOverviewRail
          messages={updatedMessages}
          layoutSnapshot={layoutSnapshot(updatedMessages)}
          minMessages={4}
          maxHeightPx={250}
          onNavigate={() => {}}
        />,
      );

      expect(
        screen.getByLabelText(/User prompt 1: Updated prompt sample/),
      ).toBeInTheDocument();
    } finally {
      document.body.removeChild(composer);
      vi.useRealTimers();
    }
  });

  it("bounds focused projection deferral when idle callbacks keep reporting low time", () => {
    const composer = document.createElement("textarea");
    composer.dataset[CONVERSATION_COMPOSER_INPUT_DATASET_KEY] = "true";
    document.body.appendChild(composer);
    const originalRequestIdleCallback = window.requestIdleCallback;
    const originalCancelIdleCallback = window.cancelIdleCallback;
    const idleCallbacks = new Map<number, IdleRequestCallback>();
    const idleOptions: Array<IdleRequestOptions | undefined> = [];
    let nextIdleHandle = 1;
    Object.defineProperty(window, "requestIdleCallback", {
      configurable: true,
      value: vi.fn(
        (
          callback: IdleRequestCallback,
          options?: IdleRequestOptions,
        ) => {
          const handle = nextIdleHandle;
          nextIdleHandle += 1;
          idleCallbacks.set(handle, callback);
          idleOptions.push(options);
          return handle;
        },
      ),
    });
    Object.defineProperty(window, "cancelIdleCallback", {
      configurable: true,
      value: vi.fn((handle: number) => {
        idleCallbacks.delete(handle);
      }),
    });

    try {
      const messages = textMessages(5);
      const updatedMessages = messages.map((message, index) =>
        index === 0 && message.type === "text"
          ? { ...message, text: "Updated prompt sample" }
          : message,
      );
      act(() => {
        composer.focus();
        fireEvent.focusIn(composer);
      });

      const { rerender } = render(
        <ConversationOverviewRail
          messages={messages}
          layoutSnapshot={layoutSnapshot(messages)}
          minMessages={4}
          maxHeightPx={250}
          onNavigate={() => {}}
        />,
      );

      rerender(
        <ConversationOverviewRail
          messages={updatedMessages}
          layoutSnapshot={layoutSnapshot(updatedMessages)}
          minMessages={4}
          maxHeightPx={250}
          onNavigate={() => {}}
        />,
      );

      expect(screen.getByLabelText(/User prompt 1: Message 1/)).toBeInTheDocument();
      expect(idleOptions[0]).toEqual({ timeout: 240 });

      act(() => {
        idleCallbacks.get(1)?.({
          didTimeout: false,
          timeRemaining: () => 0,
        } as IdleDeadline);
      });

      expect(screen.getByLabelText(/User prompt 1: Message 1/)).toBeInTheDocument();
      expect(
        screen.queryByLabelText(/User prompt 1: Updated prompt sample/),
      ).not.toBeInTheDocument();
      expect(idleOptions[1]).toEqual({ timeout: 240 });

      act(() => {
        idleCallbacks.get(2)?.({
          didTimeout: true,
          timeRemaining: () => 0,
        } as IdleDeadline);
      });

      expect(
        screen.getByLabelText(/User prompt 1: Updated prompt sample/),
      ).toBeInTheDocument();
    } finally {
      if (originalRequestIdleCallback) {
        Object.defineProperty(window, "requestIdleCallback", {
          configurable: true,
          value: originalRequestIdleCallback,
        });
      } else {
        delete (window as { requestIdleCallback?: unknown }).requestIdleCallback;
      }
      if (originalCancelIdleCallback) {
        Object.defineProperty(window, "cancelIdleCallback", {
          configurable: true,
          value: originalCancelIdleCallback,
        });
      } else {
        delete (window as { cancelIdleCallback?: unknown }).cancelIdleCallback;
      }
      document.body.removeChild(composer);
    }
  });

  it("rebuilds a focused projection through the hard timeout when idle never fires", () => {
    vi.useFakeTimers();
    const composer = document.createElement("textarea");
    composer.dataset[CONVERSATION_COMPOSER_INPUT_DATASET_KEY] = "true";
    document.body.appendChild(composer);
    const originalRequestIdleCallback = window.requestIdleCallback;
    const originalCancelIdleCallback = window.cancelIdleCallback;
    Object.defineProperty(window, "requestIdleCallback", {
      configurable: true,
      value: vi.fn(() => 1),
    });
    Object.defineProperty(window, "cancelIdleCallback", {
      configurable: true,
      value: vi.fn(),
    });

    try {
      const messages = textMessages(5);
      const updatedMessages = messages.map((message, index) =>
        index === 0 && message.type === "text"
          ? { ...message, text: "Updated prompt sample" }
          : message,
      );
      act(() => {
        composer.focus();
        fireEvent.focusIn(composer);
      });

      const { rerender } = render(
        <ConversationOverviewRail
          messages={messages}
          layoutSnapshot={layoutSnapshot(messages)}
          minMessages={4}
          maxHeightPx={250}
          onNavigate={() => {}}
        />,
      );

      rerender(
        <ConversationOverviewRail
          messages={updatedMessages}
          layoutSnapshot={layoutSnapshot(updatedMessages)}
          minMessages={4}
          maxHeightPx={250}
          onNavigate={() => {}}
        />,
      );

      expect(screen.getByLabelText(/User prompt 1: Message 1/)).toBeInTheDocument();

      act(() => {
        vi.advanceTimersByTime(240);
      });

      expect(
        screen.getByLabelText(/User prompt 1: Updated prompt sample/),
      ).toBeInTheDocument();
    } finally {
      if (originalRequestIdleCallback) {
        Object.defineProperty(window, "requestIdleCallback", {
          configurable: true,
          value: originalRequestIdleCallback,
        });
      } else {
        delete (window as { requestIdleCallback?: unknown }).requestIdleCallback;
      }
      if (originalCancelIdleCallback) {
        Object.defineProperty(window, "cancelIdleCallback", {
          configurable: true,
          value: originalCancelIdleCallback,
        });
      } else {
        delete (window as { cancelIdleCallback?: unknown }).cancelIdleCallback;
      }
      document.body.removeChild(composer);
      vi.useRealTimers();
    }
  });

  it.each([
    "url(https://example.test/marker)",
    "var(--signal-blue)",
    "linear-gradient(red, blue)",
  ])(
    "sanitizes marker pin color %s before assigning CSS custom properties",
    (color) => {
    const messages = textMessages(5);

    render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={layoutSnapshot(messages)}
        markers={[
          {
            id: "marker-1",
            messageId: "message-3",
            name: "Tampered section",
            color,
          },
        ]}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={() => {}}
      />,
    );

    expect(
      screen
        .getByLabelText("Tampered section")
        .style.getPropertyValue("--conversation-overview-marker-color"),
    ).toBe(DEFAULT_CONVERSATION_MARKER_COLOR);
    },
  );

  it("renders long same-kind runs as capped visual segments", () => {
    const messages = assistantTextMessages(100);
    const onNavigate = vi.fn();
    const { container } = render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={layoutSnapshot(messages)}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={onNavigate}
      />,
    );

    const segments = container.querySelectorAll(".conversation-overview-segment");

    expect(segments).toHaveLength(2);
    expect(
      screen.getByLabelText(/Assistant responses 1-64 \(64 messages\)/),
    ).toBeInTheDocument();
    expect(
      screen.getByLabelText(/Assistant responses 65-100 \(36 messages\)/),
    ).toBeInTheDocument();

    fireEvent.click(segments[0]);

    expect(onNavigate).toHaveBeenCalledWith(
      expect.objectContaining({
        messageId: "assistant-message-1",
        messageIndex: 0,
      }),
    );
  });

  it("uses a compact visual track for dense status-heavy sessions", () => {
    const messages = commandMessages(220);
    const onNavigate = vi.fn();
    const { container } = render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={layoutSnapshot(messages)}
        minMessages={4}
        maxHeightPx={1024}
        onNavigate={onNavigate}
      />,
    );

    const rail = screen.getByLabelText("Conversation overview");
    vi.spyOn(rail, "getBoundingClientRect").mockReturnValue({
      bottom: 1024,
      height: 1024,
      left: 0,
      right: 24,
      top: 0,
      width: 24,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });

    expect(container.querySelectorAll(".conversation-overview-segment")).toHaveLength(
      0,
    );
    expect(screen.getByTestId("conversation-overview-visual-track")).toBeInTheDocument();
    expect(
      container.querySelectorAll(".conversation-overview-visual-segment").length,
    ).toBeLessThanOrEqual(96);
    expect(rail).toHaveAttribute("role", "slider");
    expect(rail).toHaveAccessibleName("Conversation overview");
    expect(rail).toHaveAttribute("aria-orientation", "vertical");
    expect(rail).toHaveAttribute("aria-valuemin", "1");
    expect(rail).toHaveAttribute("aria-valuemax");
    expect(rail).toHaveAttribute("aria-valuenow");
    expect(rail).toHaveAttribute("aria-valuetext");
    expect(rail).toHaveAttribute("tabIndex", "0");

    fireEvent.pointerDown(rail, {
      button: 0,
      clientY: 0,
      pointerId: 11,
    });

    expect(rail).toHaveFocus();
    expect(onNavigate).toHaveBeenCalledWith(
      expect.objectContaining({
        messageId: "command-message-1",
        messageIndex: 0,
      }),
    );

    fireEvent.keyDown(rail, { key: "End" });

    expect(rail).toHaveAttribute(
      "aria-valuenow",
      rail.getAttribute("aria-valuemax"),
    );
    expect(onNavigate).toHaveBeenLastCalledWith(
      expect.objectContaining({
        messageId: "command-message-220",
        messageIndex: 219,
      }),
    );
  });

  it("switches from per-segment buttons to compact mode above the segment threshold", () => {
    const boundaryMessages = commandMessages(160);
    const { container, unmount } = render(
      <ConversationOverviewRail
        messages={boundaryMessages}
        layoutSnapshot={layoutSnapshot(boundaryMessages)}
        minMessages={4}
        maxHeightPx={1024}
        onNavigate={() => {}}
      />,
    );

    expect(container.querySelectorAll(".conversation-overview-segment")).toHaveLength(
      160,
    );
    expect(
      screen.queryByTestId("conversation-overview-visual-track"),
    ).not.toBeInTheDocument();

    unmount();

    const compactMessages = commandMessages(161);
    const compactRender = render(
      <ConversationOverviewRail
        messages={compactMessages}
        layoutSnapshot={layoutSnapshot(compactMessages)}
        minMessages={4}
        maxHeightPx={1024}
        onNavigate={() => {}}
      />,
    );

    expect(
      compactRender.container.querySelectorAll(".conversation-overview-segment"),
    ).toHaveLength(0);
    expect(screen.getByTestId("conversation-overview-visual-track")).toBeInTheDocument();
    expect(screen.getByLabelText("Conversation overview")).toHaveAttribute(
      "aria-valuemax",
      "161",
    );
  });

  it("supports compact keyboard navigation from the live viewport segment", () => {
    const messages = commandMessages(220);
    const onNavigate = vi.fn();
    const snapshot = {
      ...layoutSnapshot(messages),
      viewportTopPx: 250,
    };
    const { container } = render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={snapshot}
        minMessages={4}
        maxHeightPx={1024}
        onNavigate={onNavigate}
      />,
    );

    const rail = screen.getByLabelText("Conversation overview");
    expect(container.querySelectorAll(".conversation-overview-segment")).toHaveLength(
      0,
    );
    expect(rail).toHaveAttribute("aria-valuenow", "3");

    fireEvent.keyDown(rail, { key: "Enter" });

    expect(onNavigate).toHaveBeenLastCalledWith(
      expect.objectContaining({
        messageId: "command-message-3",
        messageIndex: 2,
      }),
    );
    expect(rail).toHaveAttribute("aria-valuenow", "3");

    fireEvent.keyDown(rail, { key: " " });

    expect(onNavigate).toHaveBeenLastCalledWith(
      expect.objectContaining({
        messageId: "command-message-3",
        messageIndex: 2,
      }),
    );

    fireEvent.keyDown(rail, { key: "ArrowDown" });

    expect(onNavigate).toHaveBeenLastCalledWith(
      expect.objectContaining({
        messageId: "command-message-4",
        messageIndex: 3,
      }),
    );
    expect(rail).toHaveAttribute("aria-valuenow", "4");

    fireEvent.keyDown(rail, { key: "ArrowUp" });

    expect(onNavigate).toHaveBeenLastCalledWith(
      expect.objectContaining({
        messageId: "command-message-3",
        messageIndex: 2,
      }),
    );
    expect(rail).toHaveAttribute("aria-valuenow", "3");

    fireEvent.keyDown(rail, { key: "Home" });

    expect(onNavigate).toHaveBeenLastCalledWith(
      expect.objectContaining({
        messageId: "command-message-1",
        messageIndex: 0,
      }),
    );
    expect(rail).toHaveAttribute("aria-valuenow", "1");
  });

  it("releases compact keyboard navigation state if the viewport never confirms", () => {
    vi.useFakeTimers();
    try {
      const messages = commandMessages(220);
      const { container } = render(
        <ConversationOverviewRail
          messages={messages}
          layoutSnapshot={layoutSnapshot(messages)}
          minMessages={4}
          maxHeightPx={1024}
          onNavigate={() => {}}
        />,
      );
      const rail = screen.getByLabelText("Conversation overview");
      const initialValue = rail.getAttribute("aria-valuenow");

      expect(container.querySelectorAll(".conversation-overview-segment")).toHaveLength(
        0,
      );

      fireEvent.keyDown(rail, { key: "End" });

      expect(rail).toHaveAttribute(
        "aria-valuenow",
        rail.getAttribute("aria-valuemax"),
      );

      act(() => {
        vi.advanceTimersByTime(800);
      });

      expect(rail).toHaveAttribute(
        "aria-valuenow",
        rail.getAttribute("aria-valuemax"),
      );

      act(() => {
        vi.advanceTimersByTime(1_200);
      });

      expect(rail).toHaveAttribute("aria-valuenow", initialValue);
    } finally {
      vi.useRealTimers();
    }
  });

  it("clears stale compact navigation timers when keyboard navigation changes", () => {
    vi.useFakeTimers();
    try {
      const messages = commandMessages(220);
      render(
        <ConversationOverviewRail
          messages={messages}
          layoutSnapshot={layoutSnapshot(messages)}
          minMessages={4}
          maxHeightPx={1024}
          onNavigate={() => {}}
        />,
      );
      const rail = screen.getByLabelText("Conversation overview");

      fireEvent.keyDown(rail, { key: "End" });
      expect(rail).toHaveAttribute(
        "aria-valuenow",
        rail.getAttribute("aria-valuemax"),
      );

      act(() => {
        vi.advanceTimersByTime(1_000);
      });
      fireEvent.keyDown(rail, { key: "Home" });
      expect(rail).toHaveAttribute("aria-valuenow", "1");

      act(() => {
        vi.advanceTimersByTime(500);
      });
      fireEvent.keyDown(rail, { key: "End" });
      expect(rail).toHaveAttribute(
        "aria-valuenow",
        rail.getAttribute("aria-valuemax"),
      );

      act(() => {
        vi.advanceTimersByTime(500);
      });
      expect(rail).toHaveAttribute(
        "aria-valuenow",
        rail.getAttribute("aria-valuemax"),
      );

      act(() => {
        vi.advanceTimersByTime(1_500);
      });
      expect(rail).not.toHaveAttribute(
        "aria-valuenow",
        rail.getAttribute("aria-valuemax") ?? "",
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("can move the viewport indicator without replacing layout geometry", () => {
    const messages = textMessages(5);
    const snapshot = layoutSnapshot(messages);
    const { messages: _layoutMessages, ...viewportSnapshot } = snapshot;
    const { rerender } = render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={snapshot}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={() => {}}
      />,
    );

    expect(screen.getByTestId("conversation-overview-viewport")).toHaveStyle({
      top: "100px",
      height: "150px",
    });

    rerender(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={snapshot}
        viewportSnapshot={{
          ...viewportSnapshot,
          viewportTopPx: 100,
        }}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={() => {}}
      />,
    );

    expect(screen.getByTestId("conversation-overview-viewport")).toHaveStyle({
      top: "50px",
      height: "150px",
    });
  });

  it("anchors the viewport indicator at the bottom from the live scroll range", () => {
    const messages = textMessages(5);
    const snapshot = layoutSnapshot(messages);
    const { messages: _layoutMessages, ...viewportSnapshot } = snapshot;

    render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={{
          ...snapshot,
          estimatedTotalHeightPx: 1_000,
        }}
        viewportSnapshot={{
          ...viewportSnapshot,
          estimatedTotalHeightPx: 800,
          viewportTopPx: 600,
          viewportHeightPx: 200,
        }}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={() => {}}
      />,
    );

    expect(screen.getByTestId("conversation-overview-viewport")).toHaveStyle({
      top: "187.5px",
      height: "62.5px",
    });
  });

  it("keeps the viewport indicator visible when the live viewport height is zero", () => {
    const messages = textMessages(5);
    const snapshot = layoutSnapshot(messages);
    const { messages: _layoutMessages, ...viewportSnapshot } = snapshot;

    render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={snapshot}
        viewportSnapshot={{
          ...viewportSnapshot,
          viewportHeightPx: 0,
        }}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={() => {}}
      />,
    );

    expect(screen.getByTestId("conversation-overview-viewport")).toHaveStyle({
      height: "8px",
    });
  });

  it("navigates to the clicked overview item", () => {
    const messages = textMessages(5);
    const onNavigate = vi.fn();

    render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={layoutSnapshot(messages)}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={onNavigate}
      />,
    );

    fireEvent.click(screen.getByLabelText(/Assistant response 2/));

    expect(onNavigate).toHaveBeenCalledTimes(1);
    expect(onNavigate).toHaveBeenCalledWith(
      expect.objectContaining({
        messageId: "message-2",
        messageIndex: 1,
      }),
    );
  });

  it("navigates on item pointer down and suppresses the follow-up click", () => {
    const messages = textMessages(5);
    const onNavigate = vi.fn();

    render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={layoutSnapshot(messages)}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={onNavigate}
      />,
    );

    const rail = screen.getByLabelText("Conversation overview");
    const secondItem = screen.getByLabelText(/Assistant response 2/);
    vi.spyOn(rail, "getBoundingClientRect").mockReturnValue({
      bottom: 250,
      height: 250,
      left: 0,
      right: 24,
      top: 0,
      width: 24,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });

    fireEvent.pointerDown(secondItem, {
      button: 0,
      clientY: 75,
      pointerId: 9,
    });
    fireEvent.pointerUp(secondItem, {
      clientY: 75,
      pointerId: 9,
    });
    fireEvent.click(secondItem);

    expect(onNavigate).toHaveBeenCalledTimes(1);
    expect(onNavigate).toHaveBeenCalledWith(
      expect.objectContaining({ messageId: "message-2" }),
    );

    fireEvent.click(screen.getByLabelText(/User prompt 3/));

    expect(onNavigate).toHaveBeenCalledTimes(2);
    expect(onNavigate).toHaveBeenLastCalledWith(
      expect.objectContaining({ messageId: "message-3" }),
    );
  });

  it("supports arrow-key navigation across overview items", () => {
    const messages = textMessages(5);

    render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={layoutSnapshot(messages)}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={() => {}}
      />,
    );

    const firstItem = screen.getByLabelText(/User prompt 1/);
    const secondItem = screen.getByLabelText(/Assistant response 2/);
    const lastItem = screen.getByLabelText(/User prompt 5/);

    expect(firstItem).toHaveAttribute("tabIndex", "0");
    expect(secondItem).toHaveAttribute("tabIndex", "-1");

    firstItem.focus();
    fireEvent.keyDown(firstItem, { key: "ArrowDown" });

    expect(secondItem).toHaveFocus();
    expect(firstItem).toHaveAttribute("tabIndex", "-1");
    expect(secondItem).toHaveAttribute("tabIndex", "0");

    fireEvent.keyDown(secondItem, { key: "End" });

    expect(lastItem).toHaveFocus();
    expect(secondItem).toHaveAttribute("tabIndex", "-1");
    expect(lastItem).toHaveAttribute("tabIndex", "0");

    fireEvent.keyDown(lastItem, { key: "Home" });

    expect(firstItem).toHaveFocus();
    expect(firstItem).toHaveAttribute("tabIndex", "0");
    expect(lastItem).toHaveAttribute("tabIndex", "-1");
  });

  it("keeps one overview segment tabbable immediately after the segment list shrinks", () => {
    const initialMessages = textMessages(5);
    const { rerender } = render(
      <ConversationOverviewRail
        messages={initialMessages}
        layoutSnapshot={layoutSnapshot(initialMessages)}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={() => {}}
      />,
    );

    const firstItem = screen.getByLabelText(/User prompt 1/);
    fireEvent.keyDown(firstItem, { key: "End" });
    expect(screen.getByLabelText(/User prompt 5/)).toHaveAttribute(
      "tabIndex",
      "0",
    );

    const shrunkMessages = textMessages(3);
    rerender(
      <ConversationOverviewRail
        messages={shrunkMessages}
        layoutSnapshot={layoutSnapshot(shrunkMessages)}
        minMessages={2}
        maxHeightPx={250}
        onNavigate={() => {}}
      />,
    );

    expect(screen.getByLabelText(/User prompt 3/)).toHaveAttribute(
      "tabIndex",
      "0",
    );
    expect(
      Array.from(
        document.querySelectorAll(".conversation-overview-segment"),
      ).filter((item) => item.getAttribute("tabIndex") === "0"),
    ).toHaveLength(1);
  });

  it("does not restore stale focus when segments shrink then grow", () => {
    const expandedMessages = textMessages(5);
    const collapsedMessages = assistantTextMessages(5);
    const { rerender } = render(
      <ConversationOverviewRail
        messages={expandedMessages}
        layoutSnapshot={layoutSnapshot(expandedMessages)}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={() => {}}
      />,
    );

    const firstItem = screen.getByLabelText(/User prompt 1/);
    fireEvent.keyDown(firstItem, { key: "End" });
    expect(screen.getByLabelText(/User prompt 5/)).toHaveAttribute(
      "tabIndex",
      "0",
    );

    rerender(
      <ConversationOverviewRail
        messages={collapsedMessages}
        layoutSnapshot={layoutSnapshot(collapsedMessages)}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={() => {}}
      />,
    );
    expect(screen.getByLabelText(/Assistant responses 1-5/)).toHaveAttribute(
      "tabIndex",
      "0",
    );

    rerender(
      <ConversationOverviewRail
        messages={expandedMessages}
        layoutSnapshot={layoutSnapshot(expandedMessages)}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={() => {}}
      />,
    );

    expect(screen.getByLabelText(/User prompt 1/)).toHaveAttribute(
      "tabIndex",
      "0",
    );
    expect(screen.getByLabelText(/User prompt 5/)).toHaveAttribute(
      "tabIndex",
      "-1",
    );
  });

  it("renders and navigates live-turn tail items", () => {
    const messages = textMessages(5);
    const onNavigate = vi.fn();

    render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={layoutSnapshot(messages)}
        minMessages={4}
        maxHeightPx={250}
        tailItems={[
          {
            id: "live-turn:session-1",
            kind: "live_turn",
            status: "running",
            estimatedHeightPx: 120,
            textSample: "Codex is working",
          },
        ]}
        onNavigate={onNavigate}
      />,
    );

    fireEvent.click(screen.getByLabelText(/Live turn 6: Codex is working/));

    expect(onNavigate).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "live_turn",
        messageId: "live-turn:session-1",
        messageIndex: 5,
      }),
    );
  });

  it("drags across the rail to scrub between overview items", () => {
    const messages = textMessages(5);
    const onNavigate = vi.fn();

    render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={layoutSnapshot(messages)}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={onNavigate}
      />,
    );

    const rail = screen.getByLabelText("Conversation overview");
    vi.spyOn(rail, "getBoundingClientRect").mockReturnValue({
      bottom: 250,
      height: 250,
      left: 0,
      right: 24,
      top: 0,
      width: 24,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });

    fireEvent.pointerDown(rail, {
      button: 0,
      clientY: 230,
      pointerId: 7,
    });
    fireEvent.pointerMove(rail, {
      clientY: 20,
      pointerId: 7,
    });
    fireEvent.pointerUp(rail, {
      pointerId: 7,
    });

    expect(onNavigate).toHaveBeenCalledTimes(2);
    expect(onNavigate).toHaveBeenNthCalledWith(
      1,
      expect.objectContaining({ messageId: "message-5" }),
    );
    expect(onNavigate).toHaveBeenNthCalledWith(
      2,
      expect.objectContaining({ messageId: "message-1" }),
    );
  });

  it("does not suppress item clicks after background rail interactions", () => {
    const messages = textMessages(5);
    const onNavigate = vi.fn();

    render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={layoutSnapshot(messages)}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={onNavigate}
      />,
    );

    const rail = screen.getByLabelText("Conversation overview");
    vi.spyOn(rail, "getBoundingClientRect").mockReturnValue({
      bottom: 250,
      height: 250,
      left: 0,
      right: 24,
      top: 0,
      width: 24,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });

    fireEvent.pointerDown(rail, {
      button: 0,
      clientY: 230,
      pointerId: 7,
    });
    fireEvent.pointerUp(rail, {
      pointerId: 7,
    });
    onNavigate.mockClear();

    fireEvent.click(screen.getByLabelText(/Assistant response 2/));

    expect(onNavigate).toHaveBeenCalledTimes(1);
    expect(onNavigate).toHaveBeenCalledWith(
      expect.objectContaining({ messageId: "message-2" }),
    );
  });

  it("does not suppress later item clicks after an item-started drag releases off item", () => {
    const messages = textMessages(5);
    const onNavigate = vi.fn();

    render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={layoutSnapshot(messages)}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={onNavigate}
      />,
    );

    const rail = screen.getByLabelText("Conversation overview");
    vi.spyOn(rail, "getBoundingClientRect").mockReturnValue({
      bottom: 250,
      height: 250,
      left: 0,
      right: 24,
      top: 0,
      width: 24,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });

    fireEvent.pointerDown(screen.getByLabelText(/User prompt 1/), {
      button: 0,
      clientY: 10,
      pointerId: 7,
    });
    fireEvent.pointerMove(rail, {
      clientY: 230,
      pointerId: 7,
    });
    fireEvent.pointerUp(rail, {
      clientY: 230,
      pointerId: 7,
    });
    onNavigate.mockClear();

    fireEvent.click(screen.getByLabelText(/Assistant response 2/));

    expect(onNavigate).toHaveBeenCalledTimes(1);
    expect(onNavigate).toHaveBeenCalledWith(
      expect.objectContaining({ messageId: "message-2" }),
    );
  });

  it("does not suppress later item clicks after an item-started drag is cancelled", () => {
    const messages = textMessages(5);
    const onNavigate = vi.fn();

    render(
      <ConversationOverviewRail
        messages={messages}
        layoutSnapshot={layoutSnapshot(messages)}
        minMessages={4}
        maxHeightPx={250}
        onNavigate={onNavigate}
      />,
    );

    const rail = screen.getByLabelText("Conversation overview");
    vi.spyOn(rail, "getBoundingClientRect").mockReturnValue({
      bottom: 250,
      height: 250,
      left: 0,
      right: 24,
      top: 0,
      width: 24,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });

    fireEvent.pointerDown(screen.getByLabelText(/User prompt 1/), {
      button: 0,
      clientY: 10,
      pointerId: 7,
    });
    fireEvent.pointerCancel(rail, {
      pointerId: 7,
    });
    onNavigate.mockClear();

    fireEvent.click(screen.getByLabelText(/Assistant response 2/));

    expect(onNavigate).toHaveBeenCalledTimes(1);
    expect(onNavigate).toHaveBeenCalledWith(
      expect.objectContaining({ messageId: "message-2" }),
    );
  });
});
