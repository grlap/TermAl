// Owns: fixed composer-adjacent activity status and in-flow queued-prompt cards.
// Does not own: session state, transcript virtualization, or scroll authority.
// Split from: ui/src/panels/AgentSessionPanel.tsx.
// Transient agent activity must never add or remove transcript height.

import { memo, useId } from "react";
import {
  DELEGATION_FAN_IN_AUTHOR_LABEL,
  isDelegationFanInText,
} from "../delegation-fan-in";
import { DelegationFanInMessage } from "../delegation-fan-in-message";
import { ExpandedPromptPanel } from "../ExpandedPromptPanel";
import {
  isPeerMessageBatch,
  LongPeerMessage,
  PEER_MESSAGE_BATCH_AUTHOR_LABEL,
  shouldCollapseLongPeerText,
} from "../long-peer-message";
import { MailboxMessageLink } from "../mailbox-message-link";
import { renderPlainTextWithSoftBreaks } from "../plain-text-wrapping";
import type { SearchHighlightTone } from "../search-highlight";
import type { PendingPrompt, Session } from "../types";
import {
  resolveSessionActivity,
  type SessionActivityOptions,
} from "./AgentSessionPanel.waiting-indicator";
import {
  MessageAttachmentList,
  MessageMeta,
  imageAttachmentSummaryLabel,
  promptCommandMetaLabel,
} from "./session-message-leaves";

export function SessionActivityStrip(options: SessionActivityOptions) {
  const activity = resolveSessionActivity(options);
  const tooltipId = useId();
  return (
    <div
      className="session-activity-strip"
      data-state={activity.state}
      data-animated={activity.animated ? "true" : "false"}
      tabIndex={0}
      aria-describedby={tooltipId}
    >
      <span className="session-activity-strip-track" aria-hidden="true">
        <span className="session-activity-strip-fill" />
      </span>
      <span
        className="visually-hidden"
        role="status"
        aria-live="polite"
        aria-atomic="true"
      >
        {activity.label}
        {activity.prompt ? `: ${activity.prompt}` : ""}
      </span>
      <div className="activity-tooltip" role="tooltip" id={tooltipId}>
        <div className="activity-tooltip-label">{activity.label}</div>
        <p>{activity.prompt ?? "No current prompt"}</p>
      </div>
    </div>
  );
}

// A paused queue is actionable transcript content, not transient activity.
// Keep Resume beside the queued prompts until the backend releases its latch.
export function QueuePausedIndicator({
  agent,
  queuedCount,
  onResume,
}: {
  agent: Session["agent"];
  queuedCount: number;
  onResume?: () => void;
}) {
  const waitingLabel =
    queuedCount === 1 ? "1 prompt waiting" : `${queuedCount} prompts waiting`;

  return (
    <article
      className="activity-card activity-card-queue-paused"
      role="status"
      aria-live="polite"
    >
      <div className="activity-pause-glyph" aria-hidden="true" />
      <div className="activity-card-copy">
        <div className="activity-card-heading">
          <div className="card-label">Queue paused</div>
        </div>
        <h3>{agent} was stopped; the queue is paused</h3>
        <p>
          {waitingLabel}. Send a new prompt or resume the queue to continue.
        </p>
      </div>
      {onResume ? (
        <button
          className="queue-resume-button"
          type="button"
          onClick={onResume}
          aria-label="Resume queued prompts"
        >
          Resume
        </button>
      ) : null}
    </article>
  );
}

export const PendingPromptCard = memo(
  function PendingPromptCard({
    prompt,
    sessionId,
    onCancel,
    onOpenMailbox,
    searchQuery = "",
    searchHighlightTone = "match",
  }: {
    prompt: PendingPrompt;
    sessionId: string;
    onCancel?: () => void;
    onOpenMailbox: (mailboxId: string) => void;
    searchQuery?: string;
    searchHighlightTone?: SearchHighlightTone;
  }) {
    const commandLabel = promptCommandMetaLabel(
      prompt.text,
      prompt.expandedText,
    );
    const isDelegationFanIn =
      !prompt.source && isDelegationFanInText(prompt.text);
    const isPeerBatch = isPeerMessageBatch(prompt.source);
    const mailboxSource =
      prompt.source?.kind === "mailbox" ? prompt.source.mailbox : null;
    const shouldCollapsePeerMessage = shouldCollapseLongPeerText(prompt);

    return (
      <article className="message-card bubble bubble-you pending-prompt-card">
        <div className="pending-prompt-header">
          <MessageMeta
            author="you"
            timestamp={prompt.timestamp}
            sourceName={
              isDelegationFanIn
                ? DELEGATION_FAN_IN_AUTHOR_LABEL
                : isPeerBatch
                  ? PEER_MESSAGE_BATCH_AUTHOR_LABEL
                  : prompt.source?.name
            }
            trailing={
              commandLabel ? (
                <span className="message-meta-tag">{commandLabel}</span>
              ) : undefined
            }
          />
          {onCancel ? (
            <button
              className="pending-prompt-dismiss"
              type="button"
              onClick={onCancel}
              aria-label="Cancel queued prompt"
            >
              x
            </button>
          ) : null}
        </div>
        {prompt.attachments && prompt.attachments.length > 0 ? (
          <MessageAttachmentList
            attachments={prompt.attachments}
            searchQuery={searchQuery}
            searchHighlightTone={searchHighlightTone}
          />
        ) : null}
        {mailboxSource ? (
          <MailboxMessageLink
            senderName={prompt.source?.name ?? "Mailbox"}
            sessionId={sessionId}
            source={mailboxSource}
            onOpenMailbox={onOpenMailbox}
          />
        ) : prompt.text ? (
          isDelegationFanIn ? (
            <DelegationFanInMessage
              text={prompt.text}
              storageKey={prompt.id}
              searchQuery={searchQuery}
              searchHighlightTone={searchHighlightTone}
            />
          ) : shouldCollapsePeerMessage ? (
            <LongPeerMessage
              text={prompt.text}
              storageKey={prompt.id}
              searchQuery={searchQuery}
              searchHighlightTone={searchHighlightTone}
            />
          ) : (
            <>
              <p className="plain-text-copy">
                {renderPlainTextWithSoftBreaks(
                  prompt.text,
                  searchQuery,
                  searchHighlightTone,
                )}
              </p>
              {prompt.expandedText ? (
                <ExpandedPromptPanel
                  expandedText={prompt.expandedText}
                  storageKey={prompt.id}
                  searchQuery={searchQuery}
                  searchHighlightTone={searchHighlightTone}
                />
              ) : null}
            </>
          )
        ) : (
          <p className="support-copy">
            {imageAttachmentSummaryLabel(prompt.attachments?.length ?? 0)}
          </p>
        )}
      </article>
    );
  },
  (previous, next) =>
    previous.prompt === next.prompt &&
    previous.sessionId === next.sessionId &&
    previous.onOpenMailbox === next.onOpenMailbox &&
    previous.searchQuery === next.searchQuery &&
    previous.searchHighlightTone === next.searchHighlightTone,
);
