import { memo, useEffect, useState } from "react";

import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";

const expandedPromptStateByStorageKey = new Map<string, boolean>();

export function isExpandedPromptOpen(storageKey?: string) {
  return storageKey ? (expandedPromptStateByStorageKey.get(storageKey) ?? false) : false;
}

export const ExpandedPromptPanel = memo(function ExpandedPromptPanel({
  expandedText,
  storageKey,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  expandedText: string;
  storageKey?: string;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [expanded, setExpanded] = useState(() =>
    storageKey ? (expandedPromptStateByStorageKey.get(storageKey) ?? false) : false,
  );
  const isExpanded = expanded || searchQuery.trim().length > 0;

  useEffect(() => {
    if (!storageKey) {
      return;
    }
    expandedPromptStateByStorageKey.set(storageKey, expanded);
  }, [expanded, storageKey]);

  return (
    <div className="prompt-expansion">
      <button
        className="ghost-button prompt-expansion-toggle"
        type="button"
        onClick={() => setExpanded((open) => !open)}
        aria-expanded={isExpanded}
      >
        {isExpanded ? "Hide expanded prompt" : "Show expanded prompt"}
      </button>
      {isExpanded ? (
        <div className="prompt-expansion-shell">
          <div className="card-label">Expanded prompt</div>
          <pre className="prompt-expansion-copy">
            {renderHighlightedText(expandedText, searchQuery, searchHighlightTone)}
          </pre>
        </div>
      ) : null}
    </div>
  );
});
