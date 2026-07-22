// Owns collapsed/expanded rendering for delegation fan-in prompt bodies. It
// deliberately does not detect fan-in protocol text or own message metadata.
// Split out of `message-cards.tsx`.

import { useEffect, useState } from "react";

import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";

// Collapsed/expanded state for delegation fan-in messages, keyed by prompt id.
// The conversation list is virtualized, so the module-level map preserves the
// user's choice when a row unmounts and remounts. Using the same key also carries
// expansion state from a queued prompt into its delivered transcript message.
const delegationFanInStateByStorageKey = new Map<string, boolean>();

export function DelegationFanInMessage({
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
      ? (delegationFanInStateByStorageKey.get(storageKey) ?? false)
      : false,
  );
  const isExpanded = expanded || searchQuery.trim().length > 0;

  useEffect(() => {
    if (!storageKey) {
      return;
    }
    delegationFanInStateByStorageKey.set(storageKey, expanded);
  }, [expanded, storageKey]);

  const firstBreak = text.indexOf("\n");
  const title = (firstBreak === -1 ? text : text.slice(0, firstBreak)).trim();
  const body =
    firstBreak === -1 ? "" : text.slice(firstBreak + 1).replace(/^\n+/, "");

  return (
    <div
      className={`expandable-session-message delegation-fan-in-message${isExpanded ? " is-expanded" : ""}`}
    >
      <p className="plain-text-copy">
        {renderHighlightedText(
          title || text,
          searchQuery,
          searchHighlightTone,
        )}
      </p>
      {body ? (
        <div className="prompt-expansion">
          <button
            className="ghost-button prompt-expansion-toggle"
            type="button"
            onClick={() => setExpanded((open) => !open)}
            aria-expanded={isExpanded}
          >
            {isExpanded
              ? "Hide delegation results"
              : "Show delegation results"}
          </button>
          {isExpanded ? (
            <div className="prompt-expansion-shell delegation-fan-in-results">
              <div className="card-label">Delegation results</div>
              <pre className="prompt-expansion-copy">
                {renderHighlightedText(body, searchQuery, searchHighlightTone)}
              </pre>
              <button
                className="ghost-button expandable-session-message-hide"
                type="button"
                onClick={() => setExpanded(false)}
                aria-label="Hide delegation results from bottom"
              >
                Hide
              </button>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
