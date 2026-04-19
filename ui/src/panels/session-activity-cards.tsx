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
//     either the "Waiting for the next chunk of output..." /
//     "Executing a command..." sub-line. When `lastPrompt` is
//     present, attaches a hover tooltip that echoes the prompt or
//     command. Emits `role="status"` + `aria-live="polite"`. The
//     command branch is gated on `lastPrompt?.trim().startsWith("/")`.
//   - `PendingPromptCard` — the user-side queued-prompt bubble
//     shown in the transcript while a prompt is waiting to be
//     submitted. Reuses `<MessageMeta>` + `<MessageAttachmentList>`
//     + `<ExpandedPromptPanel>` so search-highlight and attachment
//     chips match the rest of the transcript. Wraps its body in a
//     `memo` comparator keyed on `prompt`, `searchQuery`, and
//     `searchHighlightTone` to avoid re-rendering on unrelated
//     parent state changes.
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
// names, same copy ("Live turn", "Executing a command...",
// "Waiting for the next chunk of output...", "Cancel queued
// prompt"), same memo comparator keys.

import { memo } from "react";
import { ExpandedPromptPanel } from "../ExpandedPromptPanel";
import { renderHighlightedText, type SearchHighlightTone } from "../search-highlight";
import type { PendingPrompt, Session } from "../types";
import {
  MessageAttachmentList,
  MessageMeta,
  imageAttachmentSummaryLabel,
  promptCommandMetaLabel,
} from "./session-message-leaves";

export function RunningIndicator({
  agent,
  lastPrompt,
}: {
  agent: Session["agent"];
  lastPrompt: string | null;
}) {
  const isCommand = Boolean(lastPrompt?.trim().startsWith("/"));

  return (
    <article
      className={`activity-card activity-card-live ${lastPrompt ? "has-tooltip" : ""}`}
      role="status"
      aria-live="polite"
    >
      <div className="activity-spinner" aria-hidden="true" />
      <div className="activity-card-copy">
        <div className="activity-card-heading">
          <div className="card-label">Live turn</div>
          {isCommand ? <span className="message-meta-tag">Command</span> : null}
        </div>
        <h3>{agent} is working</h3>
        <p>{isCommand ? "Executing a command..." : "Waiting for the next chunk of output..."}</p>
      </div>
      {lastPrompt ? (
        <div className="activity-tooltip" role="tooltip">
          <div className="activity-tooltip-label">{isCommand ? "Command" : "Last prompt"}</div>
          <p>{lastPrompt}</p>
        </div>
      ) : null}
    </article>
  );
}

export const PendingPromptCard = memo(function PendingPromptCard({
  prompt,
  onCancel,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  prompt: PendingPrompt;
  onCancel: () => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const commandLabel = promptCommandMetaLabel(prompt.text, prompt.expandedText);

  return (
    <article className="message-card bubble bubble-you pending-prompt-card">
      <div className="pending-prompt-header">
        <MessageMeta
          author="you"
          timestamp={prompt.timestamp}
          trailing={
            commandLabel ? <span className="message-meta-tag">{commandLabel}</span> : undefined
          }
        />
        <button
          className="pending-prompt-dismiss"
          type="button"
          onClick={onCancel}
          aria-label="Cancel queued prompt"
          title="Cancel queued prompt"
        >
          x
        </button>
      </div>
      {prompt.attachments && prompt.attachments.length > 0 ? (
        <MessageAttachmentList
          attachments={prompt.attachments}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      ) : null}
      {prompt.text ? (
        <>
          <p className="plain-text-copy">
            {renderHighlightedText(prompt.text, searchQuery, searchHighlightTone)}
          </p>
          {prompt.expandedText ? (
            <ExpandedPromptPanel
              expandedText={prompt.expandedText}
              searchQuery={searchQuery}
              searchHighlightTone={searchHighlightTone}
            />
          ) : null}
        </>
      ) : (
        <p className="support-copy">{imageAttachmentSummaryLabel(prompt.attachments?.length ?? 0)}</p>
      )}
    </article>
  );
}, (previous, next) =>
  previous.prompt === next.prompt &&
  previous.searchQuery === next.searchQuery &&
  previous.searchHighlightTone === next.searchHighlightTone
);
