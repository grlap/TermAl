import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { listMailboxes, readMailbox } from "./api";
import { MailboxMessageLink } from "./mailbox-message-link";

vi.mock("./api", () => ({
  listMailboxes: vi.fn(),
  readMailbox: vi.fn(),
}));

const source = {
  mailboxId: "mailbox-1",
  messageId: "mailbox-message-3",
  sequence: 3,
  unreadCount: 2,
};

describe("MailboxMessageLink", () => {
  beforeEach(() => {
    vi.mocked(listMailboxes).mockReset();
    vi.mocked(readMailbox).mockReset();
  });

  it("renders one human summary line and opens the durable workspace mailbox", async () => {
    vi.mocked(listMailboxes).mockResolvedValue([
      {
        id: "mailbox-1",
        participants: [],
        latestSequence: 3,
        unreadCount: 2,
        latestMessagePreview:
          "Fable found the paging issue. Activation boilerplate stays hidden.",
      },
    ]);
    const onOpenMailbox = vi.fn();

    render(
      <MailboxMessageLink
        senderName="Termal::Fable"
        sessionId="session-codex-offline"
        source={source}
        onOpenMailbox={onOpenMailbox}
      />,
    );

    expect(
      await screen.findByText("Fable found the paging issue."),
    ).toBeTruthy();
    expect(screen.queryByText(/termal_list_mailboxes/i)).toBeNull();
    expect(screen.getByText("2 unread")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Open mailbox →" }));

    expect(onOpenMailbox).toHaveBeenCalledWith("mailbox-1");
    expect(readMailbox).not.toHaveBeenCalled();
  });

  it("remains a usable launch surface when summary refresh fails", async () => {
    vi.mocked(listMailboxes).mockRejectedValue(new Error("offline"));
    const onOpenMailbox = vi.fn();

    render(
      <MailboxMessageLink
        senderName="Termal::Fable"
        sessionId="session-codex"
        source={source}
        onOpenMailbox={onOpenMailbox}
      />,
    );

    expect(screen.getByText("Mailbox notification #3")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open mailbox →" }));
    expect(onOpenMailbox).toHaveBeenCalledWith("mailbox-1");
  });

  it("coalesces mailbox summary reads across many notification cards", async () => {
    vi.mocked(listMailboxes).mockResolvedValue([
      {
        id: "mailbox-1",
        participants: [],
        latestSequence: 3,
        unreadCount: 2,
        latestMessagePreview: "Shared preview.",
      },
    ]);

    render(
      <>
        <MailboxMessageLink
          senderName="Termal::Fable"
          sessionId="session-batch"
          source={source}
          onOpenMailbox={vi.fn()}
        />
        <MailboxMessageLink
          senderName="Termal::Fable"
          sessionId="session-batch"
          source={{ ...source, messageId: "mailbox-message-2" }}
          onOpenMailbox={vi.fn()}
        />
      </>,
    );

    expect(await screen.findAllByText("Shared preview.")).toHaveLength(2);
    expect(listMailboxes).toHaveBeenCalledTimes(1);
  });
});
