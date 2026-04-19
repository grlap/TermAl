// DOM helpers for the rendered-Markdown diff editor's caret
// tracking, focus restoration, and section-boundary navigation.
//
// What this file owns:
//   - `EditableMarkdownFocusSnapshot` — the shape the diff editor
//     captures before a re-render so focus (and scroll) can be
//     restored after React commits a new document tree. Carries
//     the caret's text offset, the scroll region + its scrollTop,
//     the original section element, and the section's segment
//     offsets (read from `data-markdown-segment-after-start` /
//     `-end`).
//   - `shouldSkipMarkdownEditableNode` — the serialization-skip
//     predicate that also drives which subtrees the caret /
//     text-walk helpers ignore. True for `<button>`,
//     `aria-hidden="true"`, or `data-markdown-serialization="skip"`
//     nodes. Used both by the focus helpers here and by the
//     `MarkdownDiffDocument` serialiser in `DiffPanel.tsx`.
//   - `captureEditableMarkdownFocusSnapshot` /
//     `scheduleEditableMarkdownFocusRestore` /
//     `restoreEditableMarkdownFocus` — the "before React commits,
//     after React commits" focus round-trip the diff editor runs
//     on every re-render that touches a section the user is
//     typing in.
//   - `shouldRestoreEditableMarkdownFocus` /
//     `findEditableMarkdownFocusTarget` — predicates / resolvers
//     that decide whether to touch focus at all and which
//     section to re-focus (exact `data-markdown-segment-after-start`
//     match → containing-range match → first editable section).
//   - Caret text-offset I/O:
//     `readEditableMarkdownCaretTextOffset`,
//     `placeCaretInEditableMarkdownSection`,
//     `placeCaretInEditableMarkdownSectionAtTextOffset`,
//     `findEditableMarkdownTextNode`,
//     `collectEditableMarkdownTextNodes`,
//     `isNodeInsideSkippedMarkdownEditableNode`.
//   - Section navigation:
//     `focusAdjacentEditableMarkdownSection` (arrow-key across
//     section boundary), `moveEditableMarkdownCaretByPage`
//     (PageUp / PageDown), `resolveEditableMarkdownPageTargetIndex`,
//     `isSelectionAtEditableSectionBoundary`,
//     `isElementVisibleWithinScrollRegion`.
//   - Small DOM utilities:
//     `readNumberDataAttribute`, `clampNumber`.
//
// What this file does NOT own:
//   - The `MarkdownDiffDocument` component itself — that stays in
//     `DiffPanel.tsx` and imports the helpers it needs.
//   - The Markdown serialiser / section renderer — those use
//     `shouldSkipMarkdownEditableNode` from here but the
//     renderer's own logic lives in `DiffPanel.tsx`.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same function
// bodies, same `data-markdown-*` attribute names, same `.markdown-
// diff-change-scroll` scroll-region selector, same `.markdown-copy`
// inner-content selector. Consumers import from here directly.

export type EditableMarkdownFocusSnapshot = {
  caretTextOffset: number | null;
  scrollRegion: HTMLElement | null;
  scrollTop: number | null;
  section: HTMLElement;
  segmentAfterEndOffset: number | null;
  segmentAfterStartOffset: number | null;
};

export function shouldSkipMarkdownEditableNode(node: HTMLElement) {
  return (
    node.tagName.toLowerCase() === "button" ||
    node.getAttribute("aria-hidden") === "true" ||
    node.dataset.markdownSerialization === "skip"
  );
}

export function captureEditableMarkdownFocusSnapshot(
  section: HTMLElement,
): EditableMarkdownFocusSnapshot {
  const scrollRegion = section.closest<HTMLElement>(".markdown-diff-change-scroll");
  return {
    caretTextOffset: readEditableMarkdownCaretTextOffset(section),
    scrollRegion,
    scrollTop: scrollRegion?.scrollTop ?? null,
    section,
    segmentAfterEndOffset: readNumberDataAttribute(section, "markdownSegmentAfterEnd"),
    segmentAfterStartOffset: readNumberDataAttribute(section, "markdownSegmentAfterStart"),
  };
}

export function scheduleEditableMarkdownFocusRestore(snapshot: EditableMarkdownFocusSnapshot) {
  window.requestAnimationFrame(() => {
    if (restoreEditableMarkdownFocus(snapshot)) {
      return;
    }

    window.setTimeout(() => {
      restoreEditableMarkdownFocus(snapshot);
    }, 0);
  });
}

export function restoreEditableMarkdownFocus(snapshot: EditableMarkdownFocusSnapshot) {
  if (!shouldRestoreEditableMarkdownFocus(snapshot)) {
    return true;
  }

  const section = findEditableMarkdownFocusTarget(snapshot);
  if (!section) {
    return false;
  }

  if (snapshot.caretTextOffset == null) {
    placeCaretInEditableMarkdownSection(section, "end");
  } else {
    placeCaretInEditableMarkdownSectionAtTextOffset(section, snapshot.caretTextOffset);
  }

  if (snapshot.scrollRegion?.isConnected && snapshot.scrollTop != null) {
    snapshot.scrollRegion.scrollTop = snapshot.scrollTop;
  }

  return true;
}

export function shouldRestoreEditableMarkdownFocus(snapshot: EditableMarkdownFocusSnapshot) {
  const activeElement = document.activeElement;
  if (!activeElement || activeElement === document.body) {
    return true;
  }

  if (snapshot.section.isConnected && activeElement === snapshot.section) {
    return true;
  }

  return false;
}

export function findEditableMarkdownFocusTarget(snapshot: EditableMarkdownFocusSnapshot) {
  if (snapshot.section.isConnected && snapshot.section.dataset.markdownEditable === "true") {
    return snapshot.section;
  }

  const scrollRegion =
    snapshot.scrollRegion?.isConnected === true
      ? snapshot.scrollRegion
      : document.querySelector<HTMLElement>(".markdown-diff-change-scroll");
  if (!scrollRegion) {
    return null;
  }

  const editableSections = Array.from(
    scrollRegion.querySelectorAll<HTMLElement>("[data-markdown-editable='true']"),
  );
  if (editableSections.length === 0) {
    return null;
  }

  const exactMatch = editableSections.find(
    (section) =>
      readNumberDataAttribute(section, "markdownSegmentAfterStart") === snapshot.segmentAfterStartOffset,
  );
  if (exactMatch) {
    return exactMatch;
  }

  const containingMatch = editableSections.find((section) => {
    const start = readNumberDataAttribute(section, "markdownSegmentAfterStart");
    const end = readNumberDataAttribute(section, "markdownSegmentAfterEnd");
    return (
      snapshot.segmentAfterStartOffset != null &&
      start != null &&
      end != null &&
      start <= snapshot.segmentAfterStartOffset &&
      snapshot.segmentAfterStartOffset <= end
    );
  });
  if (containingMatch) {
    return containingMatch;
  }

  return editableSections[0] ?? null;
}

export function readEditableMarkdownCaretTextOffset(section: HTMLElement) {
  const selection = window.getSelection();
  if (!selection || selection.rangeCount === 0 || !selection.isCollapsed) {
    return null;
  }

  const range = selection.getRangeAt(0);
  if (!section.contains(range.startContainer)) {
    return null;
  }

  const root = section.querySelector<HTMLElement>(".markdown-copy") ?? section;
  const prefixRange = document.createRange();
  prefixRange.selectNodeContents(root);
  try {
    prefixRange.setEnd(range.startContainer, range.startOffset);
  } catch {
    return null;
  }
  return prefixRange.toString().length;
}

export function readNumberDataAttribute(element: HTMLElement, key: string) {
  const value = element.dataset[key];
  if (value == null || value.length === 0) {
    return null;
  }

  const numberValue = Number(value);
  return Number.isFinite(numberValue) ? numberValue : null;
}

export function isSelectionAtEditableSectionBoundary(
  section: HTMLElement,
  boundary: "end" | "start",
) {
  const selection = window.getSelection();
  if (!selection || selection.rangeCount === 0 || !selection.isCollapsed) {
    return false;
  }

  const range = selection.getRangeAt(0);
  if (!section.contains(range.startContainer)) {
    return false;
  }

  const textNodes = collectEditableMarkdownTextNodes(section);
  const boundaryTextNode = boundary === "start" ? textNodes[0] : textNodes[textNodes.length - 1];
  if (!boundaryTextNode) {
    return true;
  }

  const caretRange = range.cloneRange();
  caretRange.collapse(true);
  const boundaryRange = document.createRange();
  if (boundary === "start") {
    boundaryRange.setStart(boundaryTextNode, 0);
    boundaryRange.collapse(true);
    return caretRange.compareBoundaryPoints(Range.START_TO_START, boundaryRange) <= 0;
  }

  boundaryRange.setStart(boundaryTextNode, boundaryTextNode.textContent?.length ?? 0);
  boundaryRange.collapse(true);
  return caretRange.compareBoundaryPoints(Range.START_TO_START, boundaryRange) >= 0;
}

export function focusAdjacentEditableMarkdownSection(
  currentSection: HTMLElement,
  direction: -1 | 1,
  beforeFocus?: () => void,
) {
  const scrollRegion = currentSection.closest<HTMLElement>(".markdown-diff-change-scroll");
  if (!scrollRegion) {
    return false;
  }

  const editableSections = Array.from(
    scrollRegion.querySelectorAll<HTMLElement>("[data-markdown-caret='true']"),
  );
  const currentIndex = editableSections.indexOf(currentSection);
  if (currentIndex < 0) {
    return false;
  }

  const targetIndex = currentIndex + direction;
  if (targetIndex < 0 || targetIndex >= editableSections.length) {
    return false;
  }

  beforeFocus?.();

  const latestEditableSections = Array.from(
    scrollRegion.querySelectorAll<HTMLElement>("[data-markdown-caret='true']"),
  );
  const latestCurrentIndex = latestEditableSections.indexOf(currentSection);
  const resolvedTargetIndex = latestCurrentIndex >= 0 ? latestCurrentIndex + direction : targetIndex;
  const nextSection = latestEditableSections[resolvedTargetIndex];
  if (!nextSection) {
    return false;
  }

  const shouldPreserveScroll = isElementVisibleWithinScrollRegion(nextSection, scrollRegion);
  const previousScrollTop = scrollRegion.scrollTop;
  placeCaretInEditableMarkdownSection(nextSection, direction > 0 ? "start" : "end");
  if (shouldPreserveScroll) {
    scrollRegion.scrollTop = previousScrollTop;
    window.requestAnimationFrame(() => {
      scrollRegion.scrollTop = previousScrollTop;
    });
  }
  return true;
}

export function moveEditableMarkdownCaretByPage(
  currentSection: HTMLElement,
  direction: -1 | 1,
  beforeMove?: () => void,
) {
  const scrollRegion = currentSection.closest<HTMLElement>(".markdown-diff-change-scroll");
  if (!scrollRegion) {
    return false;
  }

  const editableSections = Array.from(
    scrollRegion.querySelectorAll<HTMLElement>("[data-markdown-caret='true']"),
  );
  const currentIndex = editableSections.indexOf(currentSection);
  if (currentIndex < 0) {
    return false;
  }

  const targetIndex = resolveEditableMarkdownPageTargetIndex(
    editableSections,
    currentSection,
    scrollRegion,
    currentIndex,
    direction,
  );
  if (targetIndex === currentIndex) {
    return false;
  }
  const targetIndexDelta = targetIndex - currentIndex;

  beforeMove?.();

  const latestEditableSections = Array.from(
    scrollRegion.querySelectorAll<HTMLElement>("[data-markdown-caret='true']"),
  );
  const latestCurrentIndex = latestEditableSections.indexOf(currentSection);
  const resolvedTargetIndex =
    latestCurrentIndex >= 0
      ? latestCurrentIndex + targetIndexDelta
      : targetIndex;
  const nextSection = latestEditableSections[
    clampNumber(resolvedTargetIndex, 0, latestEditableSections.length - 1)
  ];
  if (!nextSection) {
    return false;
  }

  placeCaretInEditableMarkdownSection(nextSection, direction > 0 ? "start" : "end");
  nextSection.scrollIntoView?.({ block: "nearest" });
  return true;
}

export function resolveEditableMarkdownPageTargetIndex(
  editableSections: HTMLElement[],
  currentSection: HTMLElement,
  scrollRegion: HTMLElement,
  currentIndex: number,
  direction: -1 | 1,
) {
  const fallbackIndex = clampNumber(currentIndex + direction, 0, editableSections.length - 1);
  const scrollRegionRect = scrollRegion.getBoundingClientRect();
  const currentRect = currentSection.getBoundingClientRect();
  if (
    scrollRegion.clientHeight <= 0 ||
    scrollRegionRect.height <= 0 ||
    currentRect.height <= 0
  ) {
    return fallbackIndex;
  }

  const pageDistance = Math.max(scrollRegion.clientHeight * 0.85, 160);
  const currentBoundaryY =
    direction > 0
      ? currentRect.bottom + scrollRegion.scrollTop - scrollRegionRect.top
      : currentRect.top + scrollRegion.scrollTop - scrollRegionRect.top;
  const targetY = currentBoundaryY + direction * pageDistance;

  if (direction > 0) {
    const targetIndex = editableSections.findIndex((section, index) => {
      if (index <= currentIndex) {
        return false;
      }

      const sectionTop = section.getBoundingClientRect().top + scrollRegion.scrollTop - scrollRegionRect.top;
      return sectionTop >= targetY;
    });
    return targetIndex >= 0 ? targetIndex : editableSections.length - 1;
  }

  for (let index = currentIndex - 1; index >= 0; index -= 1) {
    const sectionBottom =
      editableSections[index].getBoundingClientRect().bottom +
      scrollRegion.scrollTop -
      scrollRegionRect.top;
    if (sectionBottom <= targetY) {
      return index;
    }
  }
  return 0;
}

export function isElementVisibleWithinScrollRegion(element: HTMLElement, scrollRegion: HTMLElement) {
  const elementRect = element.getBoundingClientRect();
  const scrollRegionRect = scrollRegion.getBoundingClientRect();
  return elementRect.bottom >= scrollRegionRect.top && elementRect.top <= scrollRegionRect.bottom;
}

export function placeCaretInEditableMarkdownSection(section: HTMLElement, boundary: "end" | "start") {
  section.focus({ preventScroll: true });

  const selection = window.getSelection();
  if (!selection) {
    return;
  }

  const range = document.createRange();
  const textNode =
    boundary === "start"
      ? findEditableMarkdownTextNode(section, "first")
      : findEditableMarkdownTextNode(section, "last");
  if (textNode) {
    range.setStart(textNode, boundary === "start" ? 0 : textNode.textContent?.length ?? 0);
  } else {
    range.selectNodeContents(section.querySelector(".markdown-copy") ?? section);
    range.collapse(boundary === "start");
  }
  range.collapse(true);
  selection.removeAllRanges();
  selection.addRange(range);
}

export function placeCaretInEditableMarkdownSectionAtTextOffset(section: HTMLElement, caretTextOffset: number) {
  section.focus({ preventScroll: true });

  const selection = window.getSelection();
  if (!selection) {
    return;
  }

  const range = document.createRange();
  const textNodes = collectEditableMarkdownTextNodes(section);
  let remainingOffset = Math.max(0, caretTextOffset);
  for (const textNode of textNodes) {
    const textLength = textNode.textContent?.length ?? 0;
    if (remainingOffset <= textLength) {
      range.setStart(textNode, remainingOffset);
      range.collapse(true);
      selection.removeAllRanges();
      selection.addRange(range);
      return;
    }
    remainingOffset -= textLength;
  }

  placeCaretInEditableMarkdownSection(section, "end");
}

export function findEditableMarkdownTextNode(root: Node, position: "first" | "last"): Text | null {
  const textNodes = root instanceof HTMLElement ? collectEditableMarkdownTextNodes(root) : [];
  return position === "first" ? textNodes[0] ?? null : textNodes[textNodes.length - 1] ?? null;
}

export function collectEditableMarkdownTextNodes(root: HTMLElement) {
  const textNodes: Text[] = [];
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      if (!(node instanceof Text) || !node.textContent || node.textContent.trim().length === 0) {
        return NodeFilter.FILTER_REJECT;
      }

      return isNodeInsideSkippedMarkdownEditableNode(node, root)
        ? NodeFilter.FILTER_REJECT
        : NodeFilter.FILTER_ACCEPT;
    },
  });

  let currentNode = walker.nextNode();
  while (currentNode) {
    textNodes.push(currentNode as Text);
    currentNode = walker.nextNode();
  }

  return textNodes;
}

export function isNodeInsideSkippedMarkdownEditableNode(node: Node, root: HTMLElement) {
  let current = node.parentElement;
  while (current && current !== root) {
    if (shouldSkipMarkdownEditableNode(current)) {
      return true;
    }
    current = current.parentElement;
  }
  return false;
}

export function clampNumber(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}
