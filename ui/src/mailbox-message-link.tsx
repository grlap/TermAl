// Compact human-facing launch surface for a durable neutral mailbox.
// Agent activation text remains in the stored transcript, but this renderer
// intentionally shows only mailbox metadata and never fetches message bodies.

import { useEffect, useRef, useState } from "react";

import { listMailboxes } from "./api";
import { firstSentence, mailboxAgentTone } from "./mailbox-presentation";
import type { MailboxMessageSource, MailboxSummary } from "./types";

const MAILBOX_SUMMARY_CACHE_MS = 2_500;
const mailboxSummaryRequests = new Map<
  string,
  { requestedAt: number; promise: Promise<MailboxSummary[]> }
>();

function loadMailboxSummaries(sessionId: string): Promise<MailboxSummary[]> {
  const now = Date.now();
  const cached = mailboxSummaryRequests.get(sessionId);
  if (cached && now - cached.requestedAt <= MAILBOX_SUMMARY_CACHE_MS) {
    return cached.promise;
  }

  const promise = listMailboxes(sessionId).catch((error) => {
    if (mailboxSummaryRequests.get(sessionId)?.promise === promise) {
      mailboxSummaryRequests.delete(sessionId);
    }
    throw error;
  });
  mailboxSummaryRequests.set(sessionId, { requestedAt: now, promise });
  return promise;
}

export function MailboxMessageLink({
  senderName,
  sessionId,
  source,
  onOpenMailbox,
}: {
  senderName: string;
  sessionId: string;
  source: MailboxMessageSource;
  onOpenMailbox: (mailboxId: string) => void;
}) {
  const [summary, setSummary] = useState<MailboxSummary | null>(null);
  const requestIdRef = useRef(0);

  useEffect(() => {
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    void loadMailboxSummaries(sessionId)
      .then((mailboxes) => {
        if (requestIdRef.current !== requestId) {
          return;
        }
        setSummary(
          mailboxes.find((mailbox) => mailbox.id === source.mailboxId) ?? null,
        );
      })
      .catch(() => {
        // The card remains a useful launch surface if summary refresh fails.
      });
    return () => {
      requestIdRef.current += 1;
    };
  }, [sessionId, source.mailboxId, source.sequence]);

  const preview =
    firstSentence(summary?.latestMessagePreview) ||
    `Mailbox notification #${source.sequence}`;
  const unreadCount = summary?.unreadCount ?? source.unreadCount;

  return (
    <div className="mailbox-message-link">
      <span aria-hidden="true">✉</span>
      <strong
        className={`mailbox-message-sender mailbox-message-sender-${mailboxAgentTone(senderName)}`}
      >
        {senderName}:
      </strong>
      <span className="mailbox-message-preview">{preview}</span>
      {unreadCount > 0 ? (
        <span className="mailbox-message-unread">{unreadCount} unread</span>
      ) : null}
      <button
        className="mailbox-message-open"
        type="button"
        onClick={() => onOpenMailbox(source.mailboxId)}
      >
        Open mailbox →
      </button>
    </div>
  );
}
