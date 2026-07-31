import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { listMailboxes, readMailbox } from "../api";
import { getErrorMessage } from "../app-utils";
import {
  MAILBOX_PAGE_SIZE,
  firstSentence,
  laggingMailboxParticipants,
  mailboxAgentTone,
  mailboxCursorDividerIndex,
  mailboxMessageIsProcessed,
  mailboxMessageTopic,
  mergeMailboxMessagesNewestFirst,
} from "../mailbox-presentation";
import type { MailboxMessage, MailboxSummary } from "../types";

const mailboxTimestampFormatter = new Intl.DateTimeFormat(undefined, {
  dateStyle: "medium",
  timeStyle: "short",
});

export const MAILBOX_LIVE_POLL_INTERVAL_MS = 8_000;

function formatMailboxTimestamp(timestamp: string): string {
  const date = new Date(timestamp);
  return Number.isNaN(date.valueOf())
    ? timestamp
    : mailboxTimestampFormatter.format(date);
}

export function MailboxPanel({
  mailboxId,
  sessionId,
}: {
  mailboxId: string;
  sessionId: string;
}) {
  const [summary, setSummary] = useState<MailboxSummary | null>(null);
  const [messages, setMessages] = useState<MailboxMessage[]>([]);
  const [expandedMessageIds, setExpandedMessageIds] = useState<Set<string>>(
    () => new Set(),
  );
  const [hasOlder, setHasOlder] = useState(false);
  const [loading, setLoading] = useState(true);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [backgroundError, setBackgroundError] = useState<string | null>(null);
  const requestGenerationRef = useRef(0);
  const refreshInFlightRef = useRef<number | null>(null);
  const nextRefreshRequestIdRef = useRef(1);
  const messagesRef = useRef<MailboxMessage[]>([]);
  const threadRef = useRef<HTMLDivElement | null>(null);
  const pendingPrependAnchorRef = useRef<{
    scrollHeight: number;
    scrollTop: number;
  } | null>(null);

  const refreshMailbox = useCallback(
    async ({ initial = false }: { initial?: boolean } = {}) => {
      if (refreshInFlightRef.current !== null) {
        return;
      }
      const requestId = nextRefreshRequestIdRef.current;
      nextRefreshRequestIdRef.current += 1;
      refreshInFlightRef.current = requestId;
      const generation = requestGenerationRef.current;
      if (initial) {
        setLoading(true);
        setError(null);
      } else {
        setRefreshing(true);
      }

      try {
        const mailboxes = await listMailboxes(sessionId);
        const nextSummary =
          mailboxes.find((mailbox) => mailbox.id === mailboxId) ?? null;
        if (!nextSummary) {
          throw new Error("Mailbox is no longer available to this session.");
        }
        if (requestGenerationRef.current !== generation) {
          return;
        }
        const currentMessages = messagesRef.current;
        const latestLoadedSequence = currentMessages.reduce(
          (latest, message) => Math.max(latest, message.sequence),
          0,
        );
        let nextMessages: MailboxMessage[] = [];
        if (initial || nextSummary.latestSequence > latestLoadedSequence) {
          const afterSequence = initial
            ? Math.max(0, nextSummary.latestSequence - MAILBOX_PAGE_SIZE)
            : latestLoadedSequence;
          nextMessages = await readMailbox(
            sessionId,
            mailboxId,
            afterSequence,
            MAILBOX_PAGE_SIZE,
          );
        }
        if (requestGenerationRef.current !== generation) {
          return;
        }

        const thread = threadRef.current;
        if (
          !initial &&
          nextMessages.length > 0 &&
          thread &&
          thread.scrollTop > 1
        ) {
          pendingPrependAnchorRef.current = {
            scrollHeight: thread.scrollHeight,
            scrollTop: thread.scrollTop,
          };
        }
        const merged = mergeMailboxMessagesNewestFirst([], nextMessages);
        setSummary(nextSummary);
        setMessages((current) => {
          const next = initial
            ? merged
            : mergeMailboxMessagesNewestFirst(current, nextMessages);
          messagesRef.current = next;
          return next;
        });
        if (initial) {
          setHasOlder((merged[merged.length - 1]?.sequence ?? 1) > 1);
        }
        setError(null);
        setBackgroundError(null);
      } catch (reason) {
        if (requestGenerationRef.current === generation) {
          const message = getErrorMessage(reason);
          if (initial) {
            setError(message);
          } else {
            setBackgroundError(message);
          }
        }
      } finally {
        if (refreshInFlightRef.current === requestId) {
          refreshInFlightRef.current = null;
        }
        if (requestGenerationRef.current === generation) {
          if (initial) {
            setLoading(false);
          } else {
            setRefreshing(false);
          }
        }
      }
    },
    [mailboxId, sessionId],
  );

  useEffect(() => {
    const generation = requestGenerationRef.current + 1;
    requestGenerationRef.current = generation;
    refreshInFlightRef.current = null;
    messagesRef.current = [];
    pendingPrependAnchorRef.current = null;
    setLoading(true);
    setRefreshing(false);
    setError(null);
    setBackgroundError(null);
    setMessages([]);
    setSummary(null);
    setExpandedMessageIds(new Set());

    void refreshMailbox({ initial: true });

    return () => {
      requestGenerationRef.current += 1;
      refreshInFlightRef.current = null;
    };
  }, [mailboxId, refreshMailbox, sessionId]);

  useEffect(() => {
    const refreshIfVisible = () => {
      if (!document.hidden) {
        void refreshMailbox();
      }
    };
    const intervalId = window.setInterval(
      refreshIfVisible,
      MAILBOX_LIVE_POLL_INTERVAL_MS,
    );
    window.addEventListener("focus", refreshIfVisible);
    document.addEventListener("visibilitychange", refreshIfVisible);
    return () => {
      window.clearInterval(intervalId);
      window.removeEventListener("focus", refreshIfVisible);
      document.removeEventListener("visibilitychange", refreshIfVisible);
    };
  }, [refreshMailbox]);

  useLayoutEffect(() => {
    const anchor = pendingPrependAnchorRef.current;
    const thread = threadRef.current;
    if (!anchor || !thread) {
      return;
    }
    thread.scrollTop =
      anchor.scrollTop + Math.max(0, thread.scrollHeight - anchor.scrollHeight);
    pendingPrependAnchorRef.current = null;
  }, [messages]);

  const loadOlder = useCallback(() => {
    if (loadingOlder || !hasOlder) {
      return;
    }
    const oldestSequence = messages[messages.length - 1]?.sequence ?? 1;
    const afterSequence = Math.max(0, oldestSequence - MAILBOX_PAGE_SIZE - 1);
    const generation = requestGenerationRef.current;
    setLoadingOlder(true);
    setError(null);
    void readMailbox(sessionId, mailboxId, afterSequence, MAILBOX_PAGE_SIZE)
      .then((nextMessages) => {
        if (requestGenerationRef.current !== generation) {
          return;
        }
        const olderMessages = nextMessages.filter(
          (message) => message.sequence < oldestSequence,
        );
        setMessages((current) => {
          const next = mergeMailboxMessagesNewestFirst(current, olderMessages);
          messagesRef.current = next;
          return next;
        });
        const lowestLoadedSequence = olderMessages.reduce(
          (lowest, message) => Math.min(lowest, message.sequence),
          oldestSequence,
        );
        setHasOlder(lowestLoadedSequence > 1);
      })
      .catch((reason) => {
        if (requestGenerationRef.current === generation) {
          setError(getErrorMessage(reason));
        }
      })
      .finally(() => {
        if (requestGenerationRef.current === generation) {
          setLoadingOlder(false);
        }
      });
  }, [hasOlder, loadingOlder, mailboxId, messages, sessionId]);

  const dividerEntries = useMemo(() => {
    if (!summary) {
      return new Map<number, MailboxSummary["participants"]>();
    }
    const byIndex = new Map<number, MailboxSummary["participants"]>();
    for (const participant of laggingMailboxParticipants(summary)) {
      const index = mailboxCursorDividerIndex(
        messages,
        participant.processedThrough,
      );
      if (index === null) {
        continue;
      }
      byIndex.set(index, [...(byIndex.get(index) ?? []), participant]);
    }
    return byIndex;
  }, [messages, summary]);

  const toggleExpanded = (messageId: string) => {
    setExpandedMessageIds((current) => {
      const next = new Set(current);
      if (next.has(messageId)) {
        next.delete(messageId);
      } else {
        next.add(messageId);
      }
      return next;
    });
  };

  return (
    <section className="mailbox-workspace" aria-label="Mailbox thread">
      <header className="mailbox-workspace-header">
        <div>
          <span className="card-label">Neutral mailbox</span>
          <h2>Mailbox</h2>
        </div>
        <div className="mailbox-workspace-actions">
          <span
            className={`mailbox-live-state ${backgroundError ? "stale" : "live"}`}
            title={backgroundError ?? "Mailbox refresh is current"}
          >
            {backgroundError ? "Stale" : "Live"}
          </span>
          <button
            className="ghost-button"
            type="button"
            disabled={loading || refreshing}
            onClick={() => void refreshMailbox()}
          >
            {refreshing ? "Refreshing…" : "Refresh"}
          </button>
          <span className="mailbox-workspace-id">{mailboxId}</span>
        </div>
      </header>

      {summary ? (
        <div className="mailbox-participants" aria-label="Mailbox participants">
          {summary.participants.map((participant) => {
            const caughtUp =
              participant.processedThrough >= summary.latestSequence;
            return (
              <span
                className={`mailbox-participant ${caughtUp ? "caught-up" : "lagging"}`}
                key={participant.sessionId}
              >
                <span
                  className={`mailbox-agent-dot mailbox-agent-dot-${mailboxAgentTone(participant.displayName)}`}
                  aria-hidden="true"
                />
                <strong>{participant.displayName}</strong>
                <span>
                  {participant.processedThrough}/{summary.latestSequence}
                </span>
                <span aria-hidden="true">{caughtUp ? "✓" : "●"}</span>
              </span>
            );
          })}
        </div>
      ) : null}

      {loading ? <p className="support-copy">Loading mailbox…</p> : null}
      {error ? <p className="error-copy">{error}</p> : null}
      {!loading && !error && messages.length === 0 ? (
        <p className="support-copy">No messages in this mailbox.</p>
      ) : null}

      <div className="mailbox-thread" ref={threadRef}>
        {messages.map((message, index) => {
          const expanded = expandedMessageIds.has(message.id);
          const processed = summary
            ? mailboxMessageIsProcessed(message, summary.participants)
            : false;
          const dividers = dividerEntries.get(index) ?? [];
          return (
            <div key={message.id}>
              {dividers.map((participant) => (
                <div
                  className="mailbox-cursor-divider"
                  key={`${participant.sessionId}-${participant.processedThrough}`}
                >
                  <span>
                    {participant.displayName} has processed to here (#
                    {participant.processedThrough})
                  </span>
                </div>
              ))}
              <article
                className={`mailbox-thread-row ${processed ? "processed" : "unread"} ${expanded ? "expanded" : ""}`}
              >
                <button
                  className="mailbox-thread-row-summary"
                  type="button"
                  aria-expanded={expanded}
                  onClick={() => toggleExpanded(message.id)}
                >
                  <span className="mailbox-thread-sequence">
                    #{message.sequence}
                  </span>
                  <span
                    className={`mailbox-agent-dot mailbox-agent-dot-${mailboxAgentTone(message.senderName)}`}
                    aria-hidden="true"
                  />
                  <span className="mailbox-thread-route">
                    {message.senderName}→{message.targetName}
                  </span>
                  <strong className="mailbox-thread-topic">
                    {mailboxMessageTopic(message)}
                  </strong>
                  <span className="mailbox-thread-preview">
                    {firstSentence(message.body)}
                  </span>
                  <time dateTime={message.createdAt}>
                    {formatMailboxTimestamp(message.createdAt)}
                  </time>
                  <span
                    className={`mailbox-state-chip ${processed ? "processed" : "unread"}`}
                  >
                    {processed ? "Processed" : "Unread"}
                  </span>
                </button>
                {expanded ? (
                  <div className="mailbox-thread-detail">
                    <dl>
                      <div>
                        <dt>Sequence</dt>
                        <dd>#{message.sequence}</dd>
                      </div>
                      <div>
                        <dt>Route</dt>
                        <dd>
                          {message.senderName} → {message.targetName}
                        </dd>
                      </div>
                      <div>
                        <dt>Sent</dt>
                        <dd>{formatMailboxTimestamp(message.createdAt)}</dd>
                      </div>
                      <div>
                        <dt>Wake state</dt>
                        <dd>{message.notificationState}</dd>
                      </div>
                      <div>
                        <dt>State stamp</dt>
                        <dd>{message.stateStamp ?? "—"}</dd>
                      </div>
                    </dl>
                    <pre>{message.body}</pre>
                  </div>
                ) : null}
              </article>
            </div>
          );
        })}
      </div>

      {hasOlder ? (
        <button
          className="ghost-button mailbox-load-older"
          type="button"
          disabled={loadingOlder}
          onClick={loadOlder}
        >
          {loadingOlder ? "Loading older messages…" : "Load older messages"}
        </button>
      ) : null}

      <footer>
        Read-only view. Opening, expanding, and paging never advances an agent
        cursor.
      </footer>
    </section>
  );
}
