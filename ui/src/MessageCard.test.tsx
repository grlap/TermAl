import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { MessageCard } from "./App";
import type { ApprovalMessage, DiffMessage, TextMessage, ThinkingMessage } from "./types";

describe("MessageCard", () => {
  it("shows a command badge for slash-expanded prompts", () => {
    const message: TextMessage = {
      id: "message-1",
      type: "text",
      author: "you",
      timestamp: "10:00",
      text: "/review-local",
      expandedText: "Review staged and unstaged changes.",
    };

    render(<MessageCard message={message} onApprovalDecision={vi.fn()} />);

    expect(screen.getByText("Command")).toBeInTheDocument();
  });

  it("does not show a command badge for regular user prompts", () => {
    const message: TextMessage = {
      id: "message-2",
      type: "text",
      author: "you",
      timestamp: "10:01",
      text: "Please inspect the changes.",
    };

    render(<MessageCard message={message} onApprovalDecision={vi.fn()} />);

    expect(screen.queryByText("Command")).not.toBeInTheDocument();
  });

  it("renders thinking content with markdown formatting", async () => {
    const message: ThinkingMessage = {
      id: "message-3",
      type: "thinking",
      author: "assistant",
      timestamp: "10:02",
      title: "Thinking",
      lines: ["## Summary of Changes", "1. Added markdown rendering", "- Preserved list formatting"],
    };

    render(<MessageCard message={message} onApprovalDecision={vi.fn()} />);

    expect(await screen.findByRole("heading", { name: "Summary of Changes" })).toBeInTheDocument();
    expect(screen.getByText("Added markdown rendering")).toBeInTheDocument();
    expect(screen.getByText("Preserved list formatting")).toBeInTheDocument();
  });

  it("shows relative paths and diff counts for diff cards", () => {
    const message: DiffMessage = {
      id: "message-4",
      type: "diff",
      author: "assistant",
      timestamp: "10:03",
      changeType: "edit",
      diff: [
        "@@ -1,3 +1,4 @@",
        "-before",
        "+after",
        " shared",
        "+extra",
        " shared-again",
        "-deleted",
      ].join("\n"),
      filePath: "/repo/src/app.ts",
      summary: "Updated app.ts",
    };

    render(<MessageCard message={message} onApprovalDecision={vi.fn()} workspaceRoot="/repo" />);

    expect(screen.getByText("src/app.ts")).toBeInTheDocument();
    expect(screen.queryByText("/repo/src/app.ts")).not.toBeInTheDocument();
    expect(screen.getByText("+2")).toBeInTheDocument();
    expect(screen.getByText("-2")).toBeInTheDocument();
  });

  it("shows canceled approvals as a resolved decision", () => {
    const message: ApprovalMessage = {
      id: "message-4",
      type: "approval",
      author: "assistant",
      timestamp: "10:03",
      title: "Approve edit",
      command: "apply_patch",
      detail: "Claude withdrew the request.",
      decision: "canceled",
    };

    render(<MessageCard message={message} onApprovalDecision={vi.fn()} />);

    expect(screen.getByText("Decision: canceled")).toBeInTheDocument();
  });

  it("opens assistant file links through the source callback", () => {
    const message: TextMessage = {
      id: "message-5",
      type: "text",
      author: "assistant",
      timestamp: "10:04",
      text: "[experience.tex#L63](experience.tex#L63)",
    };
    const onOpenSourceLink = vi.fn();

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "experience.tex#L63" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });

  it("autolinks bare assistant file references through the source callback", () => {
    const message: TextMessage = {
      id: "message-6",
      type: "text",
      author: "assistant",
      timestamp: "10:05",
      text: "The Microsoft scope bullet needs more evidence in experience.tex#L63.",
    };
    const onOpenSourceLink = vi.fn();

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "experience.tex#L63" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });

  it("opens inline assistant file references through the source callback", () => {
    const message: TextMessage = {
      id: "message-7",
      type: "text",
      author: "assistant",
      timestamp: "10:06",
      text: "Text like `experience.tex.#L63` should stay clickable.",
    };
    const onOpenSourceLink = vi.fn();

    render(
      <MessageCard
        message={message}
        onApprovalDecision={vi.fn()}
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot="/repo"
      />,
    );

    fireEvent.click(screen.getByRole("link", { name: "experience.tex.#L63" }));

    expect(onOpenSourceLink).toHaveBeenCalledWith({
      path: "/repo/experience.tex",
      line: 63,
      openInNewTab: false,
    });
  });
});



