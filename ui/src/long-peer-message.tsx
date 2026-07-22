// Owns compact rendering and structured batch presentation for long peer
// messages. It deliberately does not determine backend sender identity or
// queue semantics. Split out of `message-cards.tsx`.

import { useEffect, useState } from "react";

import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";
import type { MessageSource } from "./types";

const LONG_PEER_MESSAGE_CHARACTER_THRESHOLD = 640;
const LONG_PEER_MESSAGE_LINE_THRESHOLD = 12;
const LONG_PEER_MESSAGE_PREVIEW_LIMIT = 220;
const longPeerMessageStateByStorageKey = new Map<string, boolean>();

export const PEER_MESSAGE_BATCH_AUTHOR_LABEL = "Peer queue";

export function isPeerMessageBatch(
  source: MessageSource | null | undefined,
): boolean {
  return source?.kind === "peerBatch";
}

export function isLongPeerMessage(text: string): boolean {
  const trimmed = text.trim();
  return (
    trimmed.length > LONG_PEER_MESSAGE_CHARACTER_THRESHOLD ||
    trimmed.split("\n").length > LONG_PEER_MESSAGE_LINE_THRESHOLD
  );
}

function splitLongPeerMessage(text: string): { preview: string; body: string } {
  const trimmed = text.trim();
  const firstBreak = trimmed.indexOf("\n");
  if (firstBreak > 0 && firstBreak <= LONG_PEER_MESSAGE_PREVIEW_LIMIT) {
    return {
      preview: trimmed.slice(0, firstBreak).trim(),
      body: trimmed.slice(firstBreak + 1).replace(/^\n+/, ""),
    };
  }

  const preview = `${trimmed.slice(0, LONG_PEER_MESSAGE_PREVIEW_LIMIT).trimEnd()}…`;
  return {
    preview,
    body: trimmed.slice(LONG_PEER_MESSAGE_PREVIEW_LIMIT).trimStart(),
  };
}

export function LongPeerMessage({
  text,
  storageKey,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  text: string;
  storageKey?: string;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [expanded, setExpanded] = useState(() =>
    storageKey
      ? (longPeerMessageStateByStorageKey.get(storageKey) ?? false)
      : false,
  );
  const isExpanded = expanded || searchQuery.trim().length > 0;

  useEffect(() => {
    if (!storageKey) {
      return;
    }
    longPeerMessageStateByStorageKey.set(storageKey, expanded);
  }, [expanded, storageKey]);

  const { preview, body } = splitLongPeerMessage(text);

  return (
    <div
      className={`expandable-session-message long-peer-message${isExpanded ? " is-expanded" : ""}`}
    >
      <p className="plain-text-copy">
        {renderHighlightedText(preview, searchQuery, searchHighlightTone)}
      </p>
      <div className="prompt-expansion">
        <button
          className="ghost-button prompt-expansion-toggle"
          type="button"
          onClick={() => setExpanded((open) => !open)}
          aria-expanded={isExpanded}
        >
          {isExpanded ? "Hide full message" : "Show full message"}
        </button>
        {isExpanded ? (
          <div className="prompt-expansion-shell long-peer-message-results">
            <div className="card-label">Full message</div>
            <p className="plain-text-copy long-peer-message-copy">
              {renderHighlightedText(body, searchQuery, searchHighlightTone)}
            </p>
            <button
              className="ghost-button expandable-session-message-hide"
              type="button"
              onClick={() => setExpanded(false)}
              aria-label="Hide full peer message from bottom"
            >
              Hide
            </button>
          </div>
        ) : null}
      </div>
    </div>
  );
}
