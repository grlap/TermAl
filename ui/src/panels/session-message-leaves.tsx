// Small leaf React components and label helpers used by the
// `<AgentSessionPanel>` conversation view. Each piece is a thin,
// stateless building block with no session-lifecycle or
// virtualization concern of its own.
//
// What this file owns:
//   - `MessageMeta` — the compact author / timestamp row rendered at
//     the top of assistant / user messages inside the panel.
//   - `MessageAttachmentList` — the inline chip row for image
//     attachments with name + byte-size + media-type.
//   - `MessageSlot` — the search-highlightable container that wraps
//     each message card so session-find can locate and scroll to
//     matches.
//   - `PanelEmptyState` — the "Live State" empty card shown when the
//     conversation (or a filter tab) has nothing to render.
//   - Label helpers: `promptCommandMetaLabel` (returns "Command"
//     when a slash-style draft expanded into a longer message),
//     `imageAttachmentSummaryLabel` (singular / plural chip
//     summary), `formatByteSize` (B / KB / MB short form).
//   - `collectUserPromptHistory` — scans `session.messages` for
//     non-empty user-authored text prompts and returns them in
//     order for the up/down prompt-history navigator in the
//     composer.
//
// What this file does NOT own:
//   - `<AgentSessionPanel>`, `<SessionBody>`, `<SessionComposer>`,
//     `<VirtualizedConversationMessageList>`, or any of the larger
//     stateful components — all of that stays in
//     `./AgentSessionPanel.tsx`.
//   - The message-card switchboard in `../message-cards` has its
//     own local `MessageMeta` / `MessageAttachmentList` /
//     `promptCommandMetaLabel` used by the full card ladder. This
//     split deliberately keeps the panel-internal copies separate
//     from that ladder — consolidation is a future cleanup, not a
//     pure code move.
//   - Search-highlight rendering (`renderHighlightedText`) and
//     highlight-tone types — live in `../search-highlight`.
//   - Image-attachment / session types — live in `../types`.
//
// Split out of `ui/src/panels/AgentSessionPanel.tsx`. Same markup,
// same class names, same formatter boundaries (1 KB = 1024 B, MB
// threshold at 1 MiB), same "1 image attached" / "N images
// attached" pluralization.

import type { ReactNode } from "react";
import { renderHighlightedText, type SearchHighlightTone } from "../search-highlight";
import type { ImageAttachment, Session } from "../types";

export function promptCommandMetaLabel(text: string, expandedText?: string | null) {
  return expandedText && text.trim().startsWith("/") ? "Command" : null;
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

  return (
    <div className="message-meta">
      <span
        className={`message-meta-author ${isUser ? "message-meta-author-user" : "message-meta-author-agent"}`}
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
            {renderHighlightedText(attachment.fileName, searchQuery, searchHighlightTone)}
          </strong>
          <span className="message-attachment-meta">
            {formatByteSize(attachment.byteSize)} |{" "}
            {renderHighlightedText(attachment.mediaType, searchQuery, searchHighlightTone)}
          </span>
        </div>
      ))}
    </div>
  );
}

export function MessageSlot({
  children,
  itemKey,
  isSearchMatch = false,
  isSearchActive = false,
  onSearchItemMount,
}: {
  children: ReactNode;
  itemKey?: string;
  isSearchMatch?: boolean;
  isSearchActive?: boolean;
  onSearchItemMount?: (itemKey: string, node: HTMLElement | null) => void;
}) {
  if (!itemKey) {
    return <>{children}</>;
  }

  return (
    <div
      className={`message-slot${isSearchMatch ? " session-search-hit" : ""}${isSearchActive ? " session-search-hit-active" : ""}`}
      data-session-search-item-key={itemKey}
      ref={(node) => {
        onSearchItemMount?.(itemKey, node);
      }}
    >
      {children}
    </div>
  );
}

export function PanelEmptyState({ title, body }: { title: string; body: string }) {
  return (
    <article className="empty-state">
      <div className="card-label">Live State</div>
      <h3>{title}</h3>
      <p>{body}</p>
    </article>
  );
}

export function collectUserPromptHistory(session: Session) {
  return session.messages.flatMap((message) => {
    if (message.type !== "text" || message.author !== "you") {
      return [];
    }

    const prompt = message.text.trim();
    return prompt ? [prompt] : [];
  });
}

export function imageAttachmentSummaryLabel(count: number) {
  return count === 1 ? "1 image attached" : `${count} images attached`;
}

export function formatByteSize(byteSize: number) {
  if (byteSize < 1024) {
    return `${byteSize} B`;
  }

  if (byteSize < 1024 * 1024) {
    return `${(byteSize / 1024).toFixed(1)} KB`;
  }

  return `${(byteSize / (1024 * 1024)).toFixed(1)} MB`;
}
