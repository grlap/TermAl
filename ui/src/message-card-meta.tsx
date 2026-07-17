// Owns the shared message metadata row and text-message attachment chips.
// Does not own message-type switchboard rendering, Markdown/code rendering, or marker menu placement.
// Split from ui/src/message-cards.tsx.

import type { ReactNode } from "react";
import { formatByteSize } from "./app-utils";
import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";
import type { ImageAttachment } from "./types";
import { useIsMessageMetaMarkerMenuTriggerEnabled } from "./message-meta-marker-menu-context";

export function promptCommandMetaLabel(
  text: string,
  expandedText?: string | null,
) {
  return expandedText && text.trim().startsWith("/") ? "Command" : null;
}

export function MessageAttachmentList({
  attachments,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  attachments: ImageAttachment[];
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  return (
    <div className="message-attachment-list">
      {attachments.map((attachment, index) => (
        <div
          key={`${attachment.fileName}-${attachment.byteSize}-${index}`}
          className="message-attachment-chip"
        >
          <strong className="message-attachment-name">
            {renderHighlightedText(
              attachment.fileName,
              searchQuery,
              searchHighlightTone,
            )}
          </strong>
          <span className="message-attachment-meta">
            {formatByteSize(attachment.byteSize)} {"\u00b7"}{" "}
            {renderHighlightedText(
              attachment.mediaType,
              searchQuery,
              searchHighlightTone,
            )}
          </span>
        </div>
      ))}
    </div>
  );
}

export function MessageMeta({
  author,
  timestamp,
  trailing,
  sourceName,
}: {
  author: string;
  timestamp: string;
  trailing?: ReactNode;
  // Peer sender name for messages delivered via `termal_send_to_session`.
  // When set on a user-authored message the label shows this instead of "You".
  sourceName?: string | null;
}) {
  const isUser = author === "you";
  const trimmedSourceName =
    typeof sourceName === "string" ? sourceName.trim() : "";
  // A peer message keeps the user bubble styling but is labelled with the
  // sender's session name; an empty/absent name falls back to "You".
  const displayName = isUser ? trimmedSourceName || "You" : "Agent";
  const enableMarkerMenuTrigger = useIsMessageMetaMarkerMenuTriggerEnabled();
  const isMarkerMenuTrigger = enableMarkerMenuTrigger;
  const markerMenuLabel = `${displayName}, open marker actions`;
  const markerMenuTitle = isUser
    ? `Open marker actions for ${trimmedSourceName ? `${displayName}'s` : "your"} message`
    : "Open marker actions for assistant message";

  return (
    <div className="message-meta">
      <span
        className={`message-meta-author ${isUser ? "message-meta-author-user" : "message-meta-author-agent"}`}
        role={isMarkerMenuTrigger ? "button" : undefined}
        tabIndex={isMarkerMenuTrigger ? 0 : undefined}
        aria-haspopup={isMarkerMenuTrigger ? "menu" : undefined}
        aria-label={isMarkerMenuTrigger ? markerMenuLabel : undefined}
        title={isMarkerMenuTrigger ? markerMenuTitle : undefined}
        data-conversation-marker-menu-trigger={
          isMarkerMenuTrigger ? true : undefined
        }
      >
        {displayName}
      </span>
      <span className="message-meta-end">
        {trailing}
        <span>{timestamp}</span>
      </span>
    </div>
  );
}
