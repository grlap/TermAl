import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { listMailboxes, readMailbox } from "./api";
import { MailboxMessageLink } from "./mailbox-message-link";
import type { MailboxMessage } from "./types";

vi.mock("./api", () => ({
  listMailboxes: vi.fn(),
  readMailbox: vi.fn(),
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
}

function mailboxMessage(id: string, body: string): MailboxMessage {
  return {
    id,
    mailboxId: "mailbox-1",
    sequence: 1,
    senderSessionId: "session-sol",
    senderName: "Sol",
    targetSessionId: "session-fable",
    targetName: "Fable",
    createdAt: "2026-07-23T12:00:00+02:00",
    class: "routine",
    topic: null,
    body,
    notificationDisposition: "queuedBehindActiveTurn",
  };
}

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
    const reopenedRead = deferred<MailboxMessage[]>();
    vi.mocked(readMailbox)
      .mockResolvedValueOnce([
        mailboxMessage("mailbox-message-1", "First window"),
      ])
      .mockReturnValueOnce(reopenedRead.promise);
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
    await screen.findByText("First window");
    fireEvent.click(screen.getByRole("button", { name: "Hide mailbox" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Open mailbox (0 unread)" }),
    );
    await waitFor(() => expect(listMailboxes).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(readMailbox).toHaveBeenCalledTimes(2));
    expect(screen.getByText("Loading messages…")).toBeTruthy();
    expect(screen.queryByText("First window")).toBeNull();

    await act(async () => {
      reopenedRead.resolve([]);
    });
    expect(
      await screen.findByText("No messages in this mailbox."),
    ).toBeTruthy();
  });

  it("renders mailbox read failures without attempting a range read", async () => {
    vi.mocked(listMailboxes).mockRejectedValue(
      new Error("Mailbox index unavailable"),
    );
    render(
      <MailboxMessageLink
        sessionId="session-fable"
        source={{
          mailboxId: "mailbox-1",
          messageId: "mailbox-message-1",
          sequence: 1,
          unreadCount: 1,
        }}
      />,
    );

    fireEvent.click(
      screen.getByRole("button", { name: "Open mailbox (1 unread)" }),
    );

    expect(await screen.findByText("Mailbox index unavailable")).toBeTruthy();
    expect(readMailbox).not.toHaveBeenCalled();
  });

  it("ignores an obsolete read that resolves after a newer open", async () => {
    vi.mocked(listMailboxes).mockResolvedValue([
      {
        id: "mailbox-1",
        participants: [],
        latestSequence: 1,
        unreadCount: 1,
      },
    ]);
    const firstRead = deferred<MailboxMessage[]>();
    const secondRead = deferred<MailboxMessage[]>();
    vi.mocked(readMailbox)
      .mockReturnValueOnce(firstRead.promise)
      .mockReturnValueOnce(secondRead.promise);
    render(
      <MailboxMessageLink
        sessionId="session-fable"
        source={{
          mailboxId: "mailbox-1",
          messageId: "mailbox-message-1",
          sequence: 1,
          unreadCount: 1,
        }}
      />,
    );

    fireEvent.click(
      screen.getByRole("button", { name: "Open mailbox (1 unread)" }),
    );
    await waitFor(() => expect(readMailbox).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole("button", { name: "Hide mailbox" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Open mailbox (1 unread)" }),
    );
    await waitFor(() => expect(readMailbox).toHaveBeenCalledTimes(2));

    await act(async () => {
      firstRead.resolve([
        mailboxMessage("mailbox-message-stale", "Stale window"),
      ]);
    });
    expect(screen.queryByText("Stale window")).toBeNull();

    await act(async () => {
      secondRead.resolve([
        mailboxMessage("mailbox-message-fresh", "Fresh window"),
      ]);
    });
    expect(await screen.findByText("Fresh window")).toBeTruthy();
  });
});
