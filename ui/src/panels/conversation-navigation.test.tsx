import { describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  renderHook,
  screen,
  waitFor,
} from "@testing-library/react";
import {
  MessageNavigationProvider,
  MessageNavigationButtons,
  buildMessageNavigationTargetMaps,
  makeMessageNavigationLookup,
  usePagedMessageNavigation,
  type MessageNavigationContextValue,
} from "./conversation-navigation";
import type { Message } from "../types";

function makeUserPrompt(id: string, timestamp = "10:00"): Message {
  return {
    id,
    type: "text",
    author: "you",
    timestamp,
    text: `prompt ${id}`,
  };
}

function makeAssistantText(id: string, timestamp = "10:00"): Message {
  return {
    id,
    type: "text",
    author: "assistant",
    timestamp,
    text: `reply ${id}`,
  };
}

function makeDelegation(id: string, timestamp = "10:00"): Message {
  return {
    id,
    type: "parallelAgents",
    author: "assistant",
    timestamp,
    agents: [
      {
        id: `agent-${id}`,
        source: "delegation",
        title: `delegation ${id}`,
        status: "completed",
      },
    ],
  };
}

describe("buildMessageNavigationTargetMaps", () => {
  it("links delegations across non-delegation messages", () => {
    const messages: Message[] = [
      makeUserPrompt("u1"),
      makeDelegation("d1"),
      makeAssistantText("a1"),
      makeDelegation("d2"),
      makeUserPrompt("u2"),
      makeDelegation("d3"),
    ];

    const maps = buildMessageNavigationTargetMaps(messages);

    expect(maps.delegation.get("d1")).toEqual({
      prevMessageId: null,
      nextMessageId: "d2",
    });
    expect(maps.delegation.get("d2")).toEqual({
      prevMessageId: "d1",
      nextMessageId: "d3",
    });
    expect(maps.delegation.get("d3")).toEqual({
      prevMessageId: "d2",
      nextMessageId: null,
    });
  });

  it("links user prompts and ignores assistant text", () => {
    const messages: Message[] = [
      makeUserPrompt("u1"),
      makeAssistantText("a1"),
      makeUserPrompt("u2"),
      makeAssistantText("a2"),
      makeUserPrompt("u3"),
    ];

    const maps = buildMessageNavigationTargetMaps(messages);

    expect(maps.userPrompt.get("u1")).toEqual({
      prevMessageId: null,
      nextMessageId: "u2",
    });
    expect(maps.userPrompt.get("u2")).toEqual({
      prevMessageId: "u1",
      nextMessageId: "u3",
    });
    expect(maps.userPrompt.get("u3")).toEqual({
      prevMessageId: "u2",
      nextMessageId: null,
    });
    expect(maps.userPrompt.has("a1")).toBe(false);
  });

  it("returns inert targets for an unknown message id", () => {
    const messages: Message[] = [makeDelegation("d1"), makeUserPrompt("u1")];
    const maps = buildMessageNavigationTargetMaps(messages);
    const lookup = makeMessageNavigationLookup(maps);

    expect(lookup("unknown", "delegation")).toEqual({
      prevMessageId: null,
      nextMessageId: null,
    });
    expect(lookup("unknown", "userPrompt")).toEqual({
      prevMessageId: null,
      nextMessageId: null,
    });
  });

  it("marks the only delegation as inert in both directions", () => {
    const messages: Message[] = [
      makeUserPrompt("u1"),
      makeDelegation("d1"),
      makeAssistantText("a1"),
    ];

    const maps = buildMessageNavigationTargetMaps(messages);

    expect(maps.delegation.get("d1")).toEqual({
      prevMessageId: null,
      nextMessageId: null,
    });
  });
});

function renderButtons(
  messageId: string,
  kind: "delegation" | "userPrompt",
  value: MessageNavigationContextValue,
) {
  return render(
    <MessageNavigationProvider value={value}>
      <MessageNavigationButtons kind={kind} messageId={messageId} />
    </MessageNavigationProvider>,
  );
}

describe("MessageNavigationButtons", () => {
  it("renders nothing when neither prev nor next exists", () => {
    const value: MessageNavigationContextValue = {
      getNavigationTargets: () => ({
        prevMessageId: null,
        nextMessageId: null,
      }),
      hasOlderHistory: false,
      hasNewerHistory: false,
      jumpToMessageId: vi.fn(),
      navigateToAdjacentMessage: vi.fn(),
    };
    const { container } = renderButtons("d1", "delegation", value);

    expect(container.firstChild).toBeNull();
    cleanup();
  });

  it("renders both buttons and dispatches to the target message id", () => {
    const navigateToAdjacentMessage = vi.fn();
    const value: MessageNavigationContextValue = {
      getNavigationTargets: () => ({
        prevMessageId: "d1",
        nextMessageId: "d3",
      }),
      hasOlderHistory: false,
      hasNewerHistory: false,
      jumpToMessageId: vi.fn(),
      navigateToAdjacentMessage,
    };
    renderButtons("d2", "delegation", value);

    const prev = screen.getByRole("button", {
      name: "Jump to previous delegation",
    });
    const next = screen.getByRole("button", {
      name: "Jump to next delegation",
    });

    expect(prev).not.toBeDisabled();
    expect(next).not.toBeDisabled();
    expect(prev).toHaveTextContent("↑");
    expect(next).toHaveTextContent("↓");

    fireEvent.click(prev);
    expect(navigateToAdjacentMessage).toHaveBeenCalledWith(
      "d2",
      "delegation",
      "previous",
    );
    fireEvent.click(next);
    expect(navigateToAdjacentMessage).toHaveBeenCalledWith(
      "d2",
      "delegation",
      "next",
    );
    expect(navigateToAdjacentMessage).toHaveBeenCalledTimes(2);
    cleanup();
  });

  it("disables the boundary button without hiding the group", () => {
    const navigateToAdjacentMessage = vi.fn();
    const value: MessageNavigationContextValue = {
      getNavigationTargets: () => ({
        prevMessageId: null,
        nextMessageId: "u2",
      }),
      hasOlderHistory: false,
      hasNewerHistory: false,
      jumpToMessageId: vi.fn(),
      navigateToAdjacentMessage,
    };
    renderButtons("u1", "userPrompt", value);

    const prev = screen.getByRole("button", {
      name: "Jump to previous prompt",
    });
    const next = screen.getByRole("button", {
      name: "Jump to next prompt",
    });

    expect(prev).toBeDisabled();
    expect(next).not.toBeDisabled();

    fireEvent.click(prev);
    expect(navigateToAdjacentMessage).not.toHaveBeenCalled();
    fireEvent.click(next);
    expect(navigateToAdjacentMessage).toHaveBeenCalledWith(
      "u1",
      "userPrompt",
      "next",
    );
    cleanup();
  });

  it("shows page-aware prompt arrows when the resident start page has one prompt", () => {
    const navigateToAdjacentMessage = vi.fn();
    const value: MessageNavigationContextValue = {
      getNavigationTargets: () => ({
        prevMessageId: null,
        nextMessageId: null,
      }),
      hasOlderHistory: false,
      hasNewerHistory: true,
      jumpToMessageId: vi.fn(),
      navigateToAdjacentMessage,
    };
    renderButtons("u1", "userPrompt", value);

    const previous = screen.getByRole("button", {
      name: "Jump to previous prompt",
    });
    const next = screen.getByRole("button", {
      name: "Jump to next prompt",
    });
    expect(previous).toBeDisabled();
    expect(next).not.toBeDisabled();

    fireEvent.click(next);
    expect(navigateToAdjacentMessage).toHaveBeenCalledWith(
      "u1",
      "userPrompt",
      "next",
    );
    cleanup();
  });

  it("uses prompt labels for userPrompt kind", () => {
    const value: MessageNavigationContextValue = {
      getNavigationTargets: () => ({
        prevMessageId: "u1",
        nextMessageId: "u3",
      }),
      hasOlderHistory: false,
      hasNewerHistory: false,
      jumpToMessageId: vi.fn(),
      navigateToAdjacentMessage: vi.fn(),
    };
    renderButtons("u2", "userPrompt", value);

    expect(screen.getByRole("group", { name: "Prompt navigation" })).toBeTruthy();
    expect(
      screen.getByRole("button", { name: "Jump to previous prompt" }),
    ).toBeTruthy();
    expect(
      screen.getByRole("button", { name: "Jump to next prompt" }),
    ).toBeTruthy();
    cleanup();
  });
});

describe("usePagedMessageNavigation", () => {
  it("loads a newer bounded page and jumps to the adjacent off-window prompt", async () => {
    let resolveNewerPage: ((applied: boolean) => void) | null = null;
    const requestNewerPage = vi.fn(
      () =>
        new Promise<boolean>((resolve) => {
          resolveNewerPage = resolve;
        }),
    );
    const jumpToMessageId = vi.fn();
    const initialMessages = [makeUserPrompt("u1"), makeAssistantText("a1")];
    const { result, rerender } = renderHook(
      ({
        hasNewerHistory,
        messages,
      }: {
        hasNewerHistory: boolean;
        messages: Message[];
      }) =>
        usePagedMessageNavigation({
          hasNewerHistory,
          hasOlderHistory: false,
          jumpToMessageId,
          messages,
          requestNewerPage,
          requestOlderPage: vi.fn(async () => false),
          sessionId: "session-1",
        }),
      {
        initialProps: {
          hasNewerHistory: true,
          messages: initialMessages,
        },
      },
    );

    act(() => {
      result.current.navigateToAdjacentMessage("u1", "userPrompt", "next");
    });
    await waitFor(() => expect(requestNewerPage).toHaveBeenCalledTimes(1));

    rerender({
      hasNewerHistory: false,
      messages: [...initialMessages, makeUserPrompt("u2")],
    });
    await waitFor(() => expect(jumpToMessageId).toHaveBeenCalledWith("u2"));
    act(() => resolveNewerPage?.(true));
  });

  it("loads an older bounded page and jumps to the adjacent off-window prompt", async () => {
    let resolveOlderPage: ((applied: boolean) => void) | null = null;
    const requestOlderPage = vi.fn(
      () =>
        new Promise<boolean>((resolve) => {
          resolveOlderPage = resolve;
        }),
    );
    const jumpToMessageId = vi.fn();
    const initialMessages = [makeUserPrompt("u2"), makeAssistantText("a2")];
    const { result, rerender } = renderHook(
      ({
        hasOlderHistory,
        messages,
      }: {
        hasOlderHistory: boolean;
        messages: Message[];
      }) =>
        usePagedMessageNavigation({
          hasNewerHistory: false,
          hasOlderHistory,
          jumpToMessageId,
          messages,
          requestNewerPage: vi.fn(async () => false),
          requestOlderPage,
          sessionId: "session-1",
        }),
      {
        initialProps: {
          hasOlderHistory: true,
          messages: initialMessages,
        },
      },
    );

    act(() => {
      result.current.navigateToAdjacentMessage(
        "u2",
        "userPrompt",
        "previous",
      );
    });
    await waitFor(() => expect(requestOlderPage).toHaveBeenCalledTimes(1));

    rerender({
      hasOlderHistory: false,
      messages: [
        makeUserPrompt("u1"),
        makeAssistantText("a1"),
        ...initialMessages,
      ],
    });
    await waitFor(() => expect(jumpToMessageId).toHaveBeenCalledWith("u1"));
    act(() => resolveOlderPage?.(true));
  });
});
