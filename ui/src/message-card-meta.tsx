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
}: {
  author: string;
  timestamp: string;
  trailing?: ReactNode;
}) {
  const isUser = author === "you";
  const enableMarkerMenuTrigger = useIsMessageMetaMarkerMenuTriggerEnabled();
  const isMarkerMenuTrigger = enableMarkerMenuTrigger;
  const markerMenuLabel = isUser
    ? "You, open marker actions"
    : "Agent, open marker actions";
  const markerMenuTitle = isUser
    ? "Open marker actions for your message"
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
        {isUser ? "You" : "Agent"}
      </span>
      <span className="message-meta-end">
        {trailing}
        <span>{timestamp}</span>
      </span>
    </div>
  );
}
