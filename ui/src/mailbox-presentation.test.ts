import { describe, expect, it } from "vitest";

import {
  firstSentence,
  mailboxCursorDividerIndex,
  mailboxMessageIsProcessed,
} from "./mailbox-presentation";
import type { MailboxMessage, MailboxParticipant } from "./types";

function message(sequence: number): MailboxMessage {
  return {
    id: `message-${sequence}`,
    mailboxId: "mailbox-1",
    sequence,
    senderSessionId: "session-fable",
    senderName: "Termal::Fable",
    targetSessionId: "session-codex",
    targetName: "Termal::Codex",
    createdAt: "2026-07-30T12:00:00Z",
    class: "routine",
    topic: null,
    stateStamp: null,
    body: `Message ${sequence}. More detail.`,
    notificationState: "deliveredToIdleSession",
  };
}

describe("mailbox presentation rules", () => {
  it("uses the first normalized sentence for compact human summaries", () => {
    expect(firstSentence("  First   sentence. Second sentence. ")).toBe(
      "First sentence.",
    );
    expect(firstSentence("No punctuation")).toBe("No punctuation");
  });

  it("derives processed state from a genuinely lagging target cursor", () => {
    const participants: MailboxParticipant[] = [
      {
        sessionId: "session-codex",
        displayName: "Termal::Codex",
        processedThrough: 8,
      },
    ];

    expect(mailboxMessageIsProcessed(message(8), participants)).toBe(true);
    expect(mailboxMessageIsProcessed(message(9), participants)).toBe(false);
  });

  it("places a descending cursor divider immediately before processed rows", () => {
    expect(
      mailboxCursorDividerIndex(
        [message(10), message(9), message(8), message(7)],
        8,
      ),
    ).toBe(2);
  });
});
