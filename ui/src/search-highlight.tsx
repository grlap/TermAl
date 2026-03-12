import {
  Fragment,
  cloneElement,
  isValidElement,
  type ReactNode,
} from "react";

export type SearchHighlightTone = "match" | "active";

export function containsSearchMatch(text: string, rawQuery: string) {
  const query = rawQuery.trim();
  if (!query) {
    return false;
  }

  return text.toLocaleLowerCase().includes(query.toLocaleLowerCase());
}

export function renderHighlightedText(
  text: string,
  rawQuery: string,
  tone: SearchHighlightTone = "match",
): ReactNode {
  const query = rawQuery.trim();
  if (!query || !text) {
    return text;
  }

  const lowerText = text.toLocaleLowerCase();
  const lowerQuery = query.toLocaleLowerCase();
  const parts: ReactNode[] = [];
  let cursor = 0;
  let matchIndex = lowerText.indexOf(lowerQuery, cursor);
  let keyIndex = 0;

  while (matchIndex >= 0) {
    if (matchIndex > cursor) {
      parts.push(text.slice(cursor, matchIndex));
    }

    const matchedText = text.slice(matchIndex, matchIndex + query.length);
    parts.push(
      <mark
        key={`search-highlight-${keyIndex}`}
        className={`search-highlight${tone === "active" ? " is-active" : ""}`}
        aria-label={`search match: ${matchedText}`}
      >
        {matchedText}
      </mark>,
    );
    keyIndex += 1;
    cursor = matchIndex + query.length;
    matchIndex = lowerText.indexOf(lowerQuery, cursor);
  }

  if (cursor < text.length) {
    parts.push(text.slice(cursor));
  }

  return parts.length <= 1
    ? (parts[0] ?? text)
    : parts.map((part, index) =>
        typeof part === "string" ? <Fragment key={`search-text-${index}`}>{part}</Fragment> : part,
      );
}

export function highlightReactNodeText(
  node: ReactNode,
  rawQuery: string,
  tone: SearchHighlightTone = "match",
): ReactNode {
  const query = rawQuery.trim();
  if (!query) {
    return node;
  }

  if (typeof node === "string") {
    return renderHighlightedText(node, query, tone);
  }

  if (Array.isArray(node)) {
    return node.map((child, index) => (
      <Fragment key={`search-node-${index}`}>
        {highlightReactNodeText(child, query, tone)}
      </Fragment>
    ));
  }

  if (!isValidElement<{ children?: ReactNode }>(node)) {
    return node;
  }

  if (node.props.children === undefined) {
    return node;
  }

  return cloneElement(
    node,
    undefined,
    highlightReactNodeText(node.props.children, query, tone),
  );
}
