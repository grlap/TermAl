// The Find-in-session toolbar rendered above the session transcript.
//
// What this file owns:
//   - The `<div class="session-find-bar">` element, its ARIA
//     wiring (`role="search"`, `aria-label="Find in session"`),
//     and the layout of its three children: the search input, the
//     count label (`session-find-count` with `aria-live="polite"`),
//     and the Prev / Next / Close buttons.
//   - Input interactions that close over the caller's callbacks:
//     Enter / Shift+Enter triggers `onNext` / `onPrevious`,
//     Escape triggers `onClose`, typing triggers `onChange`.
//   - The empty-state copy: `"Type to search"` when there is no
//     query, `"N of M"` when there are matches, `"No matches"`
//     when the query is non-empty but the result set is empty.
//
// What this file does NOT own:
//   - The active-match bookkeeping (`activeIndex`), the match
//     results (`matches`), the query string (`query`), and the
//     `inputRef` — those are owned by the caller
//     (`SessionPaneView` in `App.tsx`), which manages find-open /
//     find-close state and wires the bar to the transcript's
//     scroll / highlight behaviour.
//   - Focus management on open — the caller drives focus through
//     the passed `inputRef`.
//   - The find algorithm itself (`buildSessionSearchMatches`,
//     etc.) — that lives in `./session-find`.
//
// Split out of `ui/src/App.tsx`. Same JSX, same class names, same
// ARIA attributes, same event handling as the inline definition
// it replaced.

import type { RefObject } from "react";
import type { SessionSearchMatch } from "./session-find";

export function SessionFindBar({
  inputRef,
  query,
  activeIndex,
  matches,
  onChange,
  onNext,
  onPrevious,
  onClose,
}: {
  inputRef: RefObject<HTMLInputElement>;
  query: string;
  activeIndex: number;
  matches: SessionSearchMatch[];
  onChange: (nextValue: string) => void;
  onNext: () => void;
  onPrevious: () => void;
  onClose: () => void;
}) {
  const hasQuery = query.trim().length > 0;
  const hasMatches = matches.length > 0;
  const currentMatch =
    hasMatches && activeIndex >= 0 ? (matches[activeIndex] ?? null) : null;
  const countLabel = !hasQuery
    ? "Type to search"
    : hasMatches
      ? `${activeIndex + 1} of ${matches.length}`
      : "No matches";

  return (
    <div
      className="session-find-bar"
      role="search"
      aria-label="Find in session"
    >
      <input
        ref={inputRef}
        className="session-find-input"
        type="search"
        value={query}
        placeholder="Find in session"
        spellCheck={false}
        onChange={(event) => onChange(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            if (event.shiftKey) {
              onPrevious();
            } else {
              onNext();
            }
            return;
          }

          if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
        }}
      />
      <span
        className="session-find-count"
        aria-live="polite"
        title={currentMatch?.snippet ?? undefined}
      >
        {countLabel}
      </span>
      <button
        className="session-find-button"
        type="button"
        onClick={onPrevious}
        disabled={!hasMatches}
      >
        Prev
      </button>
      <button
        className="session-find-button"
        type="button"
        onClick={onNext}
        disabled={!hasMatches}
      >
        Next
      </button>
      <button
        className="session-find-button session-find-close"
        type="button"
        onClick={onClose}
      >
        Close
      </button>
    </div>
  );
}
