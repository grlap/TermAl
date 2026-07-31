import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { listMailboxes, readMailbox } from "../api";
import type { MailboxMessage } from "../types";
import { MailboxPanel } from "./MailboxPanel";

vi.mock("../api", () => ({
  listMailboxes: vi.fn(),
  readMailbox: vi.fn(),
}));

function message(sequence: number): MailboxMessage {
  return {
    id: `message-${sequence}`,
    mailboxId: "mailbox-1",
    sequence,
    senderSessionId: "session-fable",
    senderName: "Termal::Fable",
    targetSessionId: "session-codex",
    targetName: "Termal::Codex",
    createdAt: `2026-07-30T12:${String(sequence % 60).padStart(2, "0")}:00Z`,
    class: "routine",
    topic: sequence === 51 ? "Pinned mailbox contract" : null,
    stateStamp: sequence === 51 ? "state-51" : null,
    body: `Message ${sequence}. Full durable body ${sequence}.`,
    notificationState: "deliveredToIdleSession",
  };
}

describe("MailboxPanel", () => {
  beforeEach(() => {
    vi.mocked(listMailboxes).mockReset();
    vi.mocked(readMailbox).mockReset();
  });

  it("renders newest-first pages with honest lagging-cursor state and never acknowledges", async () => {
    vi.mocked(listMailboxes).mockResolvedValue([
      {
        id: "mailbox-1",
        participants: [
          {
            sessionId: "session-fable",
            displayName: "Termal::Fable",
            processedThrough: 51,
          },
          {
            sessionId: "session-codex",
            displayName: "Termal::Codex",
            processedThrough: 49,
          },
        ],
        latestSequence: 51,
        unreadCount: 2,
      },
    ]);
    vi.mocked(readMailbox)
      .mockResolvedValueOnce([message(49), message(50), message(51)])
      .mockResolvedValueOnce([message(1)]);

    const { container } = render(
      <MailboxPanel mailboxId="mailbox-1" sessionId="session-codex" />,
    );

    expect(await screen.findByText("Pinned mailbox contract")).toBeTruthy();
    const rows = container.querySelectorAll(".mailbox-thread-row");
    expect(rows[0]).toHaveTextContent("#51");
    expect(rows[0]).toHaveClass("unread");
    expect(rows[2]).toHaveTextContent("#49");
    expect(rows[2]).toHaveClass("processed");
    expect(
      screen.getByText("Termal::Codex has processed to here (#49)"),
    ).toBeTruthy();
    expect(
      screen.queryByText(/Termal::Fable has processed to here/),
    ).toBeNull();

    fireEvent.click(within(rows[0] as HTMLElement).getByRole("button"));
    expect(screen.getByText("state-51")).toBeTruthy();
    expect(screen.getByText("Message 51. Full durable body 51.")).toBeTruthy();

    fireEvent.click(
      screen.getByRole("button", { name: "Load older messages" }),
    );
    await waitFor(() => expect(readMailbox).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("#1")).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: "Load older messages" }),
    ).toBeNull();

    expect(readMailbox).toHaveBeenNthCalledWith(
      1,
      "session-codex",
      "mailbox-1",
      1,
      50,
    );
    expect(readMailbox).toHaveBeenNthCalledWith(
      2,
      "session-codex",
      "mailbox-1",
      0,
      50,
    );
  });

  it("refreshes cursor-neutral state on focus and anchors a scrolled thread while prepending", async () => {
    vi.mocked(listMailboxes)
      .mockResolvedValueOnce([
        {
          id: "mailbox-1",
          participants: [
            {
              sessionId: "session-codex",
              displayName: "Termal::Codex",
              processedThrough: 1,
            },
          ],
          latestSequence: 2,
          unreadCount: 1,
        },
      ])
      .mockResolvedValueOnce([
        {
          id: "mailbox-1",
          participants: [
            {
              sessionId: "session-codex",
              displayName: "Termal::Codex",
              processedThrough: 2,
            },
          ],
          latestSequence: 3,
          unreadCount: 1,
        },
      ]);
    vi.mocked(readMailbox)
      .mockResolvedValueOnce([message(1), message(2)])
      .mockResolvedValueOnce([message(3)]);

    const { container } = render(
      <MailboxPanel mailboxId="mailbox-1" sessionId="session-codex" />,
    );
    expect(await screen.findByText("#2")).toBeTruthy();
    expect(screen.getByText("1/2")).toBeTruthy();

    const thread = container.querySelector(".mailbox-thread") as HTMLDivElement;
    Object.defineProperty(thread, "scrollHeight", {
      configurable: true,
      get: () => thread.querySelectorAll(".mailbox-thread-row").length * 100,
    });
    thread.scrollTop = 80;

    fireEvent.focus(window);

    expect(await screen.findByText("#3")).toBeTruthy();
    await waitFor(() => expect(screen.getByText("2/3")).toBeTruthy());
    expect(thread.scrollTop).toBe(180);
    expect(container.querySelectorAll(".mailbox-thread-row")[0]).toHaveTextContent(
      "#3",
    );
    expect(readMailbox).toHaveBeenNthCalledWith(
      2,
      "session-codex",
      "mailbox-1",
      2,
      50,
    );
    expect(screen.getByText("Live")).toBeTruthy();
  });

  it("keeps the last good mailbox visible and marks it stale after a refresh failure", async () => {
    vi.mocked(listMailboxes)
      .mockResolvedValueOnce([
        {
          id: "mailbox-1",
          participants: [],
          latestSequence: 1,
          unreadCount: 1,
        },
      ])
      .mockRejectedValueOnce(new Error("offline"));
    vi.mocked(readMailbox).mockResolvedValueOnce([message(1)]);

    render(<MailboxPanel mailboxId="mailbox-1" sessionId="session-codex" />);
    expect(await screen.findByText("#1")).toBeTruthy();

    fireEvent.focus(window);

    const stale = await screen.findByText("Stale");
    expect(stale).toHaveAttribute("title", "offline");
    expect(screen.getByText("#1")).toBeTruthy();
  });
});
