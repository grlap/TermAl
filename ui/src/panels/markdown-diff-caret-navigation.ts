// Caret-navigation helpers that keep keyboard-driven movement from
// landing inside a markdown diff's "removed" section, which is
// visually greyed out and not editable. When the caret would
// otherwise stay inside a removed block, these helpers redirect it
// to the nearest editable sibling section in the requested
// direction and scroll it into view.
//
// What this file owns:
//   - `getMarkdownCaretNavigationDirection` — maps arrow / page
//     key events to `-1 | 1 | null`, ignoring shortcuts with
//     modifier keys (alt / ctrl / meta).
//   - `redirectCaretOutOfRemovedMarkdownSection` — when the caret
//     sits inside a `.markdown-diff-rendered-section-removed`
//     block, finds the nearest sibling `[data-markdown-caret="true"]`
//     element (preferring the requested direction, falling back to
//     the opposite direction if no match), moves the caret there,
//     scrolls it into view, and returns `true` to signal the
//     handler should `preventDefault()` / `stopPropagation()`. No
//     redirect performed → returns `false`.
//   - `findAdjacentMarkdownCaretSection` — the private direction-
//     aware sibling scan; exported so callers that want to drive
//     caret movement from something other than a keyboard event
//     can reuse it.
//   - `closestElementFromNode` — the tiny helper that resolves the
//     closest ancestor `HTMLElement` matching a selector from a
//     `Node` (handles both `Element` and text-node starts).
//
// What this file does NOT own:
//   - Placing or restoring the caret inside an editable section
//     (`placeCaretInEditableMarkdownSection`,
//     `scheduleEditableMarkdownFocusRestore`, etc.) — those live
//     in `./editable-markdown-focus.ts`. This module calls into
//     `placeCaretInEditableMarkdownSection` but does not own it.
//   - The keyboard-handler registration itself — `DiffPanel.tsx`
//     wires these helpers into its own onKeyDown handlers and
//     decides when to consult them.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same key bindings,
// same section-class selectors, same scroll-into-view behaviour.

import type { KeyboardEvent } from "react";
import { placeCaretInEditableMarkdownSection } from "./editable-markdown-focus";

export function getMarkdownCaretNavigationDirection(event: KeyboardEvent<HTMLElement>) {
  if (event.altKey || event.ctrlKey || event.metaKey) {
    return null;
  }

  if (event.key === "ArrowDown" || event.key === "ArrowRight" || event.key === "PageDown") {
    return 1;
  }

  if (event.key === "ArrowUp" || event.key === "ArrowLeft" || event.key === "PageUp") {
    return -1;
  }

  return null;
}

export function redirectCaretOutOfRemovedMarkdownSection(
  event: KeyboardEvent<HTMLElement>,
  scrollRegion: HTMLElement | null,
  direction: -1 | 1,
) {
  if (!scrollRegion) {
    return false;
  }

  const selection = window.getSelection();
  if (!selection || selection.rangeCount === 0 || !selection.isCollapsed) {
    return false;
  }

  const range = selection.getRangeAt(0);
  const removedSection = closestElementFromNode(
    range.startContainer,
    ".markdown-diff-rendered-section-removed",
  );
  if (!removedSection || !scrollRegion.contains(removedSection)) {
    return false;
  }

  const targetSection = findAdjacentMarkdownCaretSection(scrollRegion, removedSection, direction)
    ?? findAdjacentMarkdownCaretSection(scrollRegion, removedSection, direction > 0 ? -1 : 1);
  if (!targetSection) {
    return false;
  }

  event.preventDefault();
  event.stopPropagation();
  placeCaretInEditableMarkdownSection(targetSection, direction > 0 ? "start" : "end");
  targetSection.scrollIntoView?.({ block: "nearest" });
  return true;
}

export function findAdjacentMarkdownCaretSection(
  scrollRegion: HTMLElement,
  origin: HTMLElement,
  direction: -1 | 1,
) {
  const caretSections = Array.from(
    scrollRegion.querySelectorAll<HTMLElement>("[data-markdown-caret='true']"),
  );

  if (direction > 0) {
    return caretSections.find((section) =>
      Boolean(origin.compareDocumentPosition(section) & Node.DOCUMENT_POSITION_FOLLOWING),
    ) ?? null;
  }

  for (let index = caretSections.length - 1; index >= 0; index -= 1) {
    const section = caretSections[index];
    if (section && Boolean(origin.compareDocumentPosition(section) & Node.DOCUMENT_POSITION_PRECEDING)) {
      return section;
    }
  }

  return null;
}

export function closestElementFromNode(node: Node, selector: string) {
  const element = node instanceof Element ? node : node.parentElement;
  return element?.closest<HTMLElement>(selector) ?? null;
}
