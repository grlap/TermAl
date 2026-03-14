import { memo, useState } from "react";

import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";

export const ExpandedPromptPanel = memo(function ExpandedPromptPanel({
  expandedText,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  expandedText: string;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [expanded, setExpanded] = useState(false);
  const isExpanded = expanded || searchQuery.trim().length > 0;

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
