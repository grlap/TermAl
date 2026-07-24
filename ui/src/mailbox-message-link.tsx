// Read-only durable mailbox viewer launched from a mailbox notification card.
// The notification contains metadata only; message bodies are fetched on
// demand and viewing them never advances the agent's processed cursor.

import { useEffect, useRef, useState } from "react";

import { listMailboxes, readMailbox } from "./api";
import { getErrorMessage } from "./app-utils";
import type { MailboxMessage, MailboxMessageSource } from "./types";

const mailboxTimestampFormatter = new Intl.DateTimeFormat(undefined, {
  dateStyle: "medium",
  timeStyle: "short",
});

function formatMailboxTimestamp(timestamp: string): string {
  const date = new Date(timestamp);
  return Number.isNaN(date.valueOf())
    ? timestamp
    : mailboxTimestampFormatter.format(date);
}

export function MailboxMessageLink({
  sessionId,
  source,
}: {
  sessionId: string;
  source: MailboxMessageSource;
}) {
  const [open, setOpen] = useState(false);
  const [messages, setMessages] = useState<MailboxMessage[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const mountedRef = useRef(true);
  const loadRequestRef = useRef(0);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      loadRequestRef.current += 1;
    };
  }, []);

  const toggle = () => {
    if (open) {
      setOpen(false);
      return;
    }
    setOpen(true);
    setMessages(null);
    const requestId = loadRequestRef.current + 1;
    loadRequestRef.current = requestId;
    setLoading(true);
    setError(null);
    void listMailboxes(sessionId)
      .then((mailboxes) => {
        const latestSequence =
          mailboxes.find((mailbox) => mailbox.id === source.mailboxId)
            ?.latestSequence ?? source.sequence;
        const afterSequence = Math.max(0, latestSequence - 200);
        return readMailbox(sessionId, source.mailboxId, afterSequence, 200);
      })
      .then((nextMessages) => {
        if (mountedRef.current && loadRequestRef.current === requestId) {
          setMessages(nextMessages);
        }
      })
      .catch((reason) => {
        if (mountedRef.current && loadRequestRef.current === requestId) {
          setError(getErrorMessage(reason));
        }
      })
      .finally(() => {
        if (mountedRef.current && loadRequestRef.current === requestId) {
          setLoading(false);
        }
      });
  };

  return (
    <div className="mailbox-message-link">
      <button
        className="ghost-button mailbox-message-open"
        type="button"
        onClick={toggle}
        aria-expanded={open}
      >
        {open ? "Hide mailbox" : `Open mailbox (${source.unreadCount} unread)`}
      </button>
      {open ? (
        <section className="mailbox-message-panel" aria-label="Durable mailbox">
          <header>
            <div>
              <span className="card-label">Neutral mailbox</span>
              <strong>{source.mailboxId}</strong>
            </div>
            <span className="support-copy">Read-only · durable · no agent</span>
          </header>
          {loading ? <p className="support-copy">Loading messages…</p> : null}
          {error ? <p className="error-copy">{error}</p> : null}
          {messages?.map((mailboxMessage) => (
            <article className="mailbox-message-row" key={mailboxMessage.id}>
              <div className="mailbox-message-meta">
                <strong>{mailboxMessage.senderName}</strong>
                <span>#{mailboxMessage.sequence}</span>
                <time dateTime={mailboxMessage.createdAt}>
                  {formatMailboxTimestamp(mailboxMessage.createdAt)}
                </time>
              </div>
              <p className="plain-text-copy">{mailboxMessage.body}</p>
              {mailboxMessage.topic ? (
                <span className="message-meta-tag">{mailboxMessage.topic}</span>
              ) : null}
            </article>
          ))}
          {messages && messages.length === 0 ? (
            <p className="support-copy">No messages in this mailbox.</p>
          ) : null}
        </section>
      ) : null}
    </div>
  );
}
