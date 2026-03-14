import { render, screen } from "@testing-library/react";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

import type { TextMessage } from "./types";

let MessageCard: typeof import("./App").MessageCard;
let originalQueryCommandSupported: typeof document.queryCommandSupported | undefined;

beforeAll(async () => {
  originalQueryCommandSupported = document.queryCommandSupported;
  Object.defineProperty(document, "queryCommandSupported", {
    configurable: true,
    value: vi.fn(() => false),
  });
  ({ MessageCard } = await import("./App"));
});

afterAll(() => {
  if (originalQueryCommandSupported === undefined) {
    delete (document as Partial<Document> & {
      queryCommandSupported?: typeof document.queryCommandSupported;
    }).queryCommandSupported;
    return;
  }

  Object.defineProperty(document, "queryCommandSupported", {
    configurable: true,
    value: originalQueryCommandSupported,
  });
});

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
});
