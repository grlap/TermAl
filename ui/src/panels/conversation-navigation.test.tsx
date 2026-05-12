import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import {
  MessageNavigationProvider,
  MessageNavigationButtons,
  buildMessageNavigationTargetMaps,
  makeMessageNavigationLookup,
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
      jumpToMessageId: vi.fn(),
    };
    const { container } = renderButtons("d1", "delegation", value);

    expect(container.firstChild).toBeNull();
    cleanup();
  });

  it("renders both buttons and dispatches to the target message id", () => {
    const jumpToMessageId = vi.fn();
    const value: MessageNavigationContextValue = {
      getNavigationTargets: () => ({
        prevMessageId: "d1",
        nextMessageId: "d3",
      }),
      jumpToMessageId,
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
    expect(jumpToMessageId).toHaveBeenCalledWith("d1");
    fireEvent.click(next);
    expect(jumpToMessageId).toHaveBeenCalledWith("d3");
    expect(jumpToMessageId).toHaveBeenCalledTimes(2);
    cleanup();
  });

  it("disables the boundary button without hiding the group", () => {
    const jumpToMessageId = vi.fn();
    const value: MessageNavigationContextValue = {
      getNavigationTargets: () => ({
        prevMessageId: null,
        nextMessageId: "u2",
      }),
      jumpToMessageId,
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
    expect(jumpToMessageId).not.toHaveBeenCalled();
    fireEvent.click(next);
    expect(jumpToMessageId).toHaveBeenCalledWith("u2");
    cleanup();
  });

  it("uses prompt labels for userPrompt kind", () => {
    const value: MessageNavigationContextValue = {
      getNavigationTargets: () => ({
        prevMessageId: "u1",
        nextMessageId: "u3",
      }),
      jumpToMessageId: vi.fn(),
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
