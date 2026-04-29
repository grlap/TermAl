// Pointer/clipboard geometry helpers for the rendered-Markdown diff
// editor. These are pure DOM/Range utilities used by clipboard handlers
// (`copy`, `cut`) and pointer-driven drop placement on the
// `contentEditable` section. They intentionally have no React or
// component-tree dependencies so they can be unit-tested or reused by
// future clipboard plumbing.
//
// What this file owns:
//   - `setDropCaretFromPoint` — places the document caret at a screen
//     coordinate inside the section, using `caretPositionFromPoint` /
//     `caretRangeFromPoint` (vendor-prefixed shape via the
//     `DocumentWithCaretRangeFromPoint` type below). Used by the
//     section's `onDrop` handler so dropped text lands where the
//     pointer was, not where the previous selection was.
//   - `getSelectionRangeInsideSection` — narrows the document
//     selection to a `Range` strictly contained by the given section
//     element. Returns `null` for collapsed selections, no selection,
//     or selections that escape the section's subtree (the latter is
//     why we do not just call `selection.getRangeAt(0)`).
//   - `rangeCoversNodeContents` — returns `true` iff the range fully
//     covers the contents of the given node (inclusive boundary
//     comparisons). Used as a "select all the section's body" probe
//     so `serializeSelectedMarkdown` below can fall back to the
//     section's source markdown verbatim instead of round-tripping
//     through the `<div>` clone path that occasionally drops trailing
//     blank lines.
//   - `serializeSelectedMarkdown` — converts the selected portion of
//     the section's HTML back into Markdown. Clones the range into a
//     scratch `<div>`, runs `serializeEditableMarkdownSection` over
//     it, and falls back to either the full segment markdown (if the
//     range covers the whole markdown body) or `range.toString()`
//     when the round-trip yielded an empty string (e.g., the
//     selection is purely punctuation/whitespace inside an empty
//     paragraph).
//
// What this file does NOT own:
//   - The HTML-to-Markdown serializer itself
//     (`serializeEditableMarkdownSection`) and the paste sanitiser
//     (`insertSanitizedMarkdownPaste`) — those live in
//     `./markdown-diff-edit-pipeline`.
//   - The React clipboard handlers (`handleCopy`, `handleCut`,
//     `handleDrop`) that consume these helpers — those live in
//     `./markdown-diff-change-section`.
//   - Caret navigation between sections (arrow / page / boundary
//     redirects) — that lives in `./markdown-diff-caret-navigation`
//     and `./editable-markdown-focus`.
//   - Segment normalisation / model layer — that lives in
//     `./markdown-diff-segments`.
//
// Split out of `ui/src/panels/markdown-diff-change-section.tsx` as a
// pure code move: same behaviour, same DOM contract, same fallback
// rules. Re-imported by the original file via the named exports
// below.

import { serializeEditableMarkdownSection } from "./markdown-diff-edit-pipeline";

// Cross-vendor caret-from-point shape. `caretPositionFromPoint` is the
// standardised name; `caretRangeFromPoint` is the WebKit/Blink legacy
// alias that some tested browser versions still expose (and Firefox
// historically did not). Either being present is enough; missing both
// just falls through to a no-op (the section keeps its previous
// selection).
type DocumentWithCaretRangeFromPoint = Document & {
  caretPositionFromPoint?: (
    x: number,
    y: number,
  ) => { offsetNode: Node; offset: number } | null;
  caretRangeFromPoint?: (x: number, y: number) => Range | null;
};

export function setDropCaretFromPoint(
  section: HTMLElement,
  clientX: number,
  clientY: number,
) {
  const ownerDocument = section.ownerDocument as DocumentWithCaretRangeFromPoint;
  let range: Range | null = null;
  const caretPosition = ownerDocument.caretPositionFromPoint?.(clientX, clientY);
  if (caretPosition) {
    range = ownerDocument.createRange();
    range.setStart(caretPosition.offsetNode, caretPosition.offset);
    range.collapse(true);
  } else {
    range = ownerDocument.caretRangeFromPoint?.(clientX, clientY) ?? null;
  }
  if (!range) {
    return;
  }

  const startContainer = range.startContainer;
  if (startContainer !== section && !section.contains(startContainer)) {
    return;
  }

  const selection = ownerDocument.getSelection();
  selection?.removeAllRanges();
  selection?.addRange(range);
}

export function getSelectionRangeInsideSection(section: HTMLElement) {
  const selection = section.ownerDocument.getSelection();
  if (!selection || selection.rangeCount === 0 || selection.isCollapsed) {
    return null;
  }

  const range = selection.getRangeAt(0);
  const containsNode = (node: Node) => node === section || section.contains(node);
  if (!containsNode(range.startContainer) || !containsNode(range.endContainer)) {
    return null;
  }

  return range;
}

export function rangeCoversNodeContents(range: Range, node: HTMLElement) {
  const fullRange = node.ownerDocument.createRange();
  fullRange.selectNodeContents(node);
  return (
    range.compareBoundaryPoints(Range.START_TO_START, fullRange) <= 0 &&
    range.compareBoundaryPoints(Range.END_TO_END, fullRange) >= 0
  );
}

export function serializeSelectedMarkdown(
  range: Range,
  fallbackMarkdown: string,
  fallbackScope: HTMLElement,
) {
  const ownerDocument = range.commonAncestorContainer.ownerDocument ?? document;
  const container = ownerDocument.createElement("div");
  container.append(range.cloneContents());
  const markdown = serializeEditableMarkdownSection(container);
  if (markdown.trim().length > 0) {
    return markdown;
  }

  const markdownRoot =
    fallbackScope.querySelector<HTMLElement>(".markdown-copy") ?? fallbackScope;
  return rangeCoversNodeContents(range, markdownRoot)
    ? fallbackMarkdown
    : range.toString();
}
