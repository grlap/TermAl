// Owns non-destructive soft-wrap opportunities for plain transcript text.
// It does not style bubbles or decide which messages use plain-text rendering.
// `<wbr>` keeps copied text byte-for-byte clean while giving long paths and
// URLs preferable break points before CSS falls back to arbitrary wrapping.

import {
  cloneElement,
  isValidElement,
  type ReactNode,
} from "react";

import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";

const MIN_SOFT_BREAK_TOKEN_LENGTH = 24;
const SOFT_BREAK_AFTER = new Set(["/", "\\", "_", "-", "?", "&", "="]);

function collectSoftBreakOffsets(text: string): Set<number> {
  const offsets = new Set<number>();
  const tokenPattern = new RegExp(
    `\\S{${MIN_SOFT_BREAK_TOKEN_LENGTH},}`,
    "g",
  );

  for (const tokenMatch of text.matchAll(tokenPattern)) {
    const token = tokenMatch[0];
    const tokenStart = tokenMatch.index;
    const tokenEnd = tokenStart + token.length;

    for (let index = tokenStart; index < tokenEnd - 1; index += 1) {
      if (SOFT_BREAK_AFTER.has(text[index])) {
        offsets.add(index + 1);
      }
    }
  }

  return offsets;
}

function addSoftBreaksToHighlightedText(
  node: ReactNode,
  offsets: ReadonlySet<number>,
): ReactNode {
  let textOffset = 0;
  let breakKey = 0;

  function visit(current: ReactNode): ReactNode {
    if (typeof current === "string") {
      const startOffset = textOffset;
      const endOffset = startOffset + current.length;
      textOffset = endOffset;

      const localOffsets = Array.from(offsets)
        .filter((offset) => offset > startOffset && offset <= endOffset)
        .sort((left, right) => left - right);

      if (localOffsets.length === 0) {
        return current;
      }

      const parts: ReactNode[] = [];
      let cursor = 0;
      for (const offset of localOffsets) {
        const localOffset = offset - startOffset;
        parts.push(current.slice(cursor, localOffset));
        parts.push(<wbr key={`plain-text-break-${breakKey}`} />);
        breakKey += 1;
        cursor = localOffset;
      }
      parts.push(current.slice(cursor));
      return parts;
    }

    if (Array.isArray(current)) {
      return current.map((child) => visit(child));
    }

    if (
      isValidElement<{ children?: ReactNode }>(current) &&
      current.props.children !== undefined
    ) {
      return cloneElement(current, undefined, visit(current.props.children));
    }

    return current;
  }

  return visit(node);
}

export function renderPlainTextWithSoftBreaks(
  text: string,
  searchQuery = "",
  searchHighlightTone: SearchHighlightTone = "match",
): ReactNode {
  const highlightedText = renderHighlightedText(
    text,
    searchQuery,
    searchHighlightTone,
  );
  const offsets = collectSoftBreakOffsets(text);
  if (offsets.size === 0) {
    return highlightedText;
  }

  return addSoftBreaksToHighlightedText(highlightedText, offsets);
}
