import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { listMailboxes, readMailbox } from "./api";
import { MailboxMessageLink } from "./mailbox-message-link";

vi.mock("./api", () => ({
  listMailboxes: vi.fn(),
  readMailbox: vi.fn(),
}));

describe("MailboxMessageLink", () => {
  beforeEach(() => {
    vi.mocked(listMailboxes).mockReset();
    vi.mocked(readMailbox).mockReset();
  });

  it("fetches durable messages only when opened and renders a neutral read-only view", async () => {
    vi.mocked(listMailboxes).mockResolvedValue([
      {
        id: "mailbox-1",
        participants: [],
        latestSequence: 350,
        unreadCount: 1,
      },
    ]);
    vi.mocked(readMailbox).mockResolvedValue([
      {
        id: "mailbox-message-1",
        mailboxId: "mailbox-1",
        sequence: 301,
        senderSessionId: "session-sol",
        senderName: "Sol",
        targetSessionId: "session-fable",
        targetName: "Fable",
        createdAt: "2026-07-23T12:00:00+02:00",
        class: "routine",
        topic: "architecture",
        body: "Committed mailbox body",
        notificationDisposition: "queuedBehindActiveTurn",
      },
    ]);

    const view = render(
      <MailboxMessageLink
        sessionId="session-fable"
        source={{
          mailboxId: "mailbox-1",
          messageId: "mailbox-message-1",
          sequence: 301,
          unreadCount: 1,
        }}
      />,
    );

    expect(readMailbox).not.toHaveBeenCalled();
    fireEvent.click(
      screen.getByRole("button", { name: "Open mailbox (1 unread)" }),
    );

    await waitFor(() =>
      expect(readMailbox).toHaveBeenCalledWith(
        "session-fable",
        "mailbox-1",
        150,
        200,
      ),
    );
    expect(await screen.findByText("Committed mailbox body")).toBeTruthy();
    expect(screen.getByText("Read-only · durable · no agent")).toBeTruthy();
    expect(screen.getByText("architecture")).toBeTruthy();
    const timestamp = view.container.querySelector("time");
    expect(timestamp?.getAttribute("datetime")).toBe(
      "2026-07-23T12:00:00+02:00",
    );
    expect(timestamp?.textContent).not.toBe("2026-07-23T12:00:00+02:00");
  });

  it("refetches the newest mailbox window when the viewer is reopened", async () => {
    vi.mocked(listMailboxes).mockResolvedValue([
      {
        id: "mailbox-1",
        participants: [],
        latestSequence: 1,
        unreadCount: 0,
      },
    ]);
    vi.mocked(readMailbox).mockResolvedValue([]);
    render(
      <MailboxMessageLink
        sessionId="session-fable"
        source={{
          mailboxId: "mailbox-1",
          messageId: "mailbox-message-1",
          sequence: 1,
          unreadCount: 0,
        }}
      />,
    );

    fireEvent.click(
      screen.getByRole("button", { name: "Open mailbox (0 unread)" }),
    );
    await screen.findByText("No messages in this mailbox.");
    fireEvent.click(screen.getByRole("button", { name: "Hide mailbox" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Open mailbox (0 unread)" }),
    );
    await waitFor(() => expect(listMailboxes).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(readMailbox).toHaveBeenCalledTimes(2));
  });
});
