import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { MessageCard } from "./message-cards";
import type { ApprovalMessage } from "./types";

vi.mock("./api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./api")>()),
  listMailboxes: vi.fn().mockResolvedValue([]),
}));

describe("multiline approval detail", () => {
  it.each([
    { searchQuery: "", button: "Approve", decision: "accepted" },
    { searchQuery: "first", button: "Reject", decision: "rejected" },
  ] as const)(
    "preserves diff indentation and wrapping while choosing $decision",
    ({ searchQuery, button, decision }) => {
      const detail = [
        `File: ${"nested/".repeat(40)}example.py`,
        "After:",
        "if enabled:",
        "    first()",
        "second()",
        "",
        "Before:",
        "if enabled:",
        "    previous()",
      ].join("\n");
      const message: ApprovalMessage = {
        id: "opaque-acp-approval",
        type: "approval",
        author: "assistant",
        timestamp: "10:00",
        title: "Edit example.py",
        command: "Edit",
        detail,
        decision: "pending",
        supportedDecisions: ["accepted", "rejected"],
      };
      const onApprovalDecision = vi.fn();
      const { container } = render(
        <MessageCard
          message={message}
          searchQuery={searchQuery}
          onApprovalDecision={onApprovalDecision}
          onUserInputSubmit={vi.fn()}
        />,
      );
      const preview = container.querySelector(".approval-card .support-copy");
      expect(preview).toBeVisible();
      expect(preview?.textContent).toBe(detail);
      const presentation = getComputedStyle(preview as HTMLElement);
      expect(presentation.whiteSpace).toBe("pre-wrap");
      expect(presentation.overflowWrap).toBe("anywhere");
      if (searchQuery) {
        expect(preview?.querySelector("mark")?.textContent).toBe(searchQuery);
      }
      expect(
        screen.queryByRole("button", { name: "Approve for session" }),
      ).not.toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: button }));
      expect(onApprovalDecision).toHaveBeenCalledExactlyOnceWith(
        message.id,
        decision,
      );
    },
  );
});
