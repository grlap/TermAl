// Small React cards the session panel renders as the live-state
// footer of a conversation: the "Live turn" / "Command" spinner
// shown while an agent is actively working, and the "Queued
// prompt" bubble shown when a user-authored prompt is waiting for
// the current turn to finish.
//
// What this file owns:
//   - `RunningIndicator` — the activity card that reports the
//     agent's live turn status. Renders a pulsing dot, the agent
//     name ("Claude is working" / "Codex is working" etc.), and
//     latest user prompt as the turn context. When `lastPrompt` is
//     present, attaches a hover/focus tooltip with the complete prompt.
//     Agent commands stay in transcript command cards and never replace
//     the user intent summarized here. Emits `role="status"` +
//     `aria-live="polite"`.
//   - `PendingPromptCard` — the user-side queued-prompt bubble
//     shown in the transcript while a prompt is waiting to be
//     submitted. Reuses `<MessageMeta>` + `<MessageAttachmentList>`
//     + `<ExpandedPromptPanel>` so search-highlight and attachment
//     chips match the rest of the transcript. Structured mailbox
//     wake-ups reuse `<MailboxMessageLink>` instead of exposing the
//     agent activation boilerplate. Wraps its body in a `memo`
//     comparator keyed on its render data and actions to avoid
//     re-rendering on unrelated parent state changes.
//
// What this file does NOT own:
//   - `<ExpandedPromptPanel>` and the expansion logic — lives in
//     `../ExpandedPromptPanel`.
//   - `<MessageMeta>` / `<MessageAttachmentList>` /
//     `promptCommandMetaLabel` / `imageAttachmentSummaryLabel` —
//     live in `./session-message-leaves`.
//   - Search-highlight rendering (`renderHighlightedText`,
//     `SearchHighlightTone`) — lives in `../search-highlight`.
//   - The panel shell, virtualisation, composer, or any stateful
//     session wiring — all of that stays in
//     `./AgentSessionPanel.tsx`.
//
// Split out of `ui/src/panels/AgentSessionPanel.tsx`. Same class
// names and the same queued-prompt behavior.

import { memo } from "react";
import {
  DELEGATION_FAN_IN_AUTHOR_LABEL,
  isDelegationFanInText,
} from "../delegation-fan-in";
import { DelegationFanInMessage } from "../delegation-fan-in-message";
import { ExpandedPromptPanel } from "../ExpandedPromptPanel";
import {
  isLongPeerMessage,
  isPeerMessageBatch,
  LongPeerMessage,
  PEER_MESSAGE_BATCH_AUTHOR_LABEL,
} from "../long-peer-message";
import { MailboxMessageLink } from "../mailbox-message-link";
import { renderHighlightedText, type SearchHighlightTone } from "../search-highlight";
import type {
  PendingPrompt,
  Session,
  SessionLiveActivity,
} from "../types";
import {
  MessageAttachmentList,
  MessageMeta,
  imageAttachmentSummaryLabel,
  promptCommandMetaLabel,
} from "./session-message-leaves";

export function RunningIndicator({
  agent,
  activity,
  lastPrompt,
}: {
  agent: Session["agent"];
  activity?: SessionLiveActivity | null;
  lastPrompt: string | null;
}) {
  const prompt = activity?.prompt.trim() || lastPrompt?.trim() || null;
  const tooltipText = prompt;

  return (
    <article
      className={`activity-card activity-card-live ${tooltipText ? "has-tooltip" : ""}`}
      role="status"
      aria-live="polite"
      tabIndex={tooltipText ? 0 : undefined}
    >
      <div className="activity-spinner" aria-hidden="true" />
      <div className="activity-card-copy">
        <div className="activity-card-heading">
          <div className="card-label">Live turn</div>
        </div>
        <h3>{agent} is working</h3>
        <p className={prompt ? "activity-card-prompt" : undefined}>
          {prompt ?? "Working on the current turn..."}
        </p>
      </div>
      {tooltipText ? (
        <div className="activity-tooltip" role="tooltip">
          <div className="activity-tooltip-label">Current prompt</div>
          <p>{tooltipText}</p>
        </div>
      ) : null}
    </article>
  );
}

export const PendingPromptCard = memo(function PendingPromptCard({
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
  const commandLabel = promptCommandMetaLabel(prompt.text, prompt.expandedText);
  const isDelegationFanIn =
    !prompt.source && isDelegationFanInText(prompt.text);
  const isPeerBatch = isPeerMessageBatch(prompt.source);
  const mailboxSource =
    prompt.source?.kind === "mailbox" ? prompt.source.mailbox : null;
  const shouldCollapsePeerMessage =
    (Boolean(prompt.source) || isPeerBatch) && isLongPeerMessage(prompt.text);

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
            commandLabel ? <span className="message-meta-tag">{commandLabel}</span> : undefined
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
              {renderHighlightedText(prompt.text, searchQuery, searchHighlightTone)}
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
        <p className="support-copy">{imageAttachmentSummaryLabel(prompt.attachments?.length ?? 0)}</p>
      )}
    </article>
  );
}, (previous, next) =>
  previous.prompt === next.prompt &&
  previous.sessionId === next.sessionId &&
  previous.onOpenMailbox === next.onOpenMailbox &&
  previous.searchQuery === next.searchQuery &&
  previous.searchHighlightTone === next.searchHighlightTone
);
