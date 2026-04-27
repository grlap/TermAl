// Pure helpers that resolve and verify document offsets when the
// Markdown diff editor commits a single section back into the full
// document.
//
// What this file owns:
//   - `RenderedMarkdownSectionCommit` — the per-section commit
//     payload the Markdown diff editor queues up. Carries both
//     the pre-edit `segment` (whose offsets are relative to the
//     snapshot the renderer saw) and the current in-memory
//     `currentSegment`, plus the `sourceContent` the edit was
//     made against and the `nextMarkdown` the user typed.
//   - `MarkdownDocumentRange` — `{ start, end }` offset pair into
//     the full Markdown document.
//   - `resolveRenderedMarkdownCommitRange` — tries four strategies
//     to locate the current position of a committed section in
//     the latest document: (1) the original pre-edit offsets if
//     they still match; (2) the original offsets shifted across
//     the source → current diff; (3) a nearest-neighbour search
//     by content; (4) the current-segment offsets if they match.
//     Returns `null` when none of them do.
//   - `markdownRangeMatches` — returns true when slicing the
//     document at `[start, end)` produces the expected Markdown
//     substring.
//   - `hasOverlappingMarkdownCommitRanges` — detects overlap
//     across a batch of pending section commits. Flags both
//     strict overlap (non-adjacent ranges that cross) and
//     zero-length touch cases (two insertion points at the same
//     offset, or an insertion point touching a non-empty range)
//     because the subsequent descending-by-start splice would
//     apply both writes at the same offset in unspecified order.
//   - `mapMarkdownRangeAcrossContentChange` — shifts a range from
//     source-content coordinates into current-content coordinates
//     when the edit is outside the range (prefix/suffix-only
//     changes). Returns `null` for changes that straddle the
//     range.
//   - `findClosestMarkdownRange` — nearest-neighbour content
//     search for a target Markdown substring, minimising distance
//     from a preferred start offset.
//
// What this file does NOT own:
//   - `MarkdownDiffDocumentSegment` — the segment type itself
//     lives in `./markdown-diff-segments`.
//   - The commit-queue orchestration in `DiffPanel.tsx`; this
//     module provides the pure math it runs over each commit.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same types, same
// function bodies, same thresholds; consumers (including
// `DiffPanel.test.tsx`) import directly from here.

import {
  normalizeMarkdownDocumentLineEndings,
  type MarkdownDiffDocumentSegment,
} from "./markdown-diff-segments";

export type RenderedMarkdownSectionCommit = {
  // Diff-side section commits can use the current segment as a last-resort
  // locator because the current segment is derived from the same rendered diff
  // model. Full-document preview callers must pass false: their current visible
  // segment may already have drifted from the persisted source.
  allowCurrentSegmentFallback: boolean;
  currentSegment: MarkdownDiffDocumentSegment;
  segment: MarkdownDiffDocumentSegment;
  nextMarkdown: string;
  sourceContent: string;
  // Called only after the parent accepts and applies the commit. Rejected
  // commits must keep their child draft state intact so the user can retry.
  // `resetRenderedContent: false` clears the child draft refs while preserving
  // the current contentEditable DOM for no-op/equivalent commits.
  onApplied?: (options?: { resetRenderedContent?: boolean }) => void;
};

export type MarkdownDocumentRange = {
  end: number;
  start: number;
};

export function resolveRenderedMarkdownCommitRange(
  currentContent: string,
  commit: RenderedMarkdownSectionCommit,
): MarkdownDocumentRange | null {
  const normalizedSourceContent = normalizeMarkdownDocumentLineEndings(
    commit.sourceContent,
  );
  const originalRange = {
    start: commit.segment.afterStartOffset,
    end: commit.segment.afterEndOffset,
  };
  if (markdownRangeMatches(currentContent, originalRange, commit.segment.markdown)) {
    return originalRange;
  }

  const mappedRange = mapMarkdownRangeAcrossContentChange(
    normalizedSourceContent,
    currentContent,
    originalRange,
  );
  if (mappedRange && markdownRangeMatches(currentContent, mappedRange, commit.segment.markdown)) {
    return mappedRange;
  }

  const searchedRange = findClosestMarkdownRange(
    currentContent,
    commit.segment.markdown,
    mappedRange?.start ?? originalRange.start,
  );
  if (searchedRange) {
    return searchedRange;
  }

  if (commit.allowCurrentSegmentFallback) {
    const currentRange = {
      start: commit.currentSegment.afterStartOffset,
      end: commit.currentSegment.afterEndOffset,
    };
    if (markdownRangeMatches(currentContent, currentRange, commit.currentSegment.markdown)) {
      return currentRange;
    }
  }

  return null;
}

export function markdownRangeMatches(
  content: string,
  range: MarkdownDocumentRange,
  expected: string,
) {
  return (
    range.start >= 0 &&
    range.end >= range.start &&
    range.end <= content.length &&
    content.slice(range.start, range.end) === expected
  );
}

export function hasOverlappingMarkdownCommitRanges(
  commits: Array<{
    commit: RenderedMarkdownSectionCommit;
    range: MarkdownDocumentRange;
  }>,
) {
  const sortedRanges = [...commits].sort((left, right) => left.range.start - right.range.start);
  for (let index = 1; index < sortedRanges.length; index += 1) {
    const previous = sortedRanges[index - 1];
    const current = sortedRanges[index];
    if (!previous || !current) {
      continue;
    }
    // Strict `<` flags non-adjacent overlap for non-empty ranges. The
    // additional `<=` branch rejects two zero-length ranges that share
    // an insertion point (e.g., both resolve to `[10, 10)`), and
    // rejects a zero-length range that touches a non-empty sibling —
    // in both cases the subsequent descending-by-start splice would
    // apply both writes at the same offset in unspecified order and
    // silently garble the document.
    const isZeroLengthTouch =
      current.range.start === previous.range.end &&
      (current.range.start === current.range.end || previous.range.start === previous.range.end);
    if (current.range.start < previous.range.end || isZeroLengthTouch) {
      return true;
    }
  }

  return false;
}

export function mapMarkdownRangeAcrossContentChange(
  sourceContent: string,
  currentContent: string,
  range: MarkdownDocumentRange,
): MarkdownDocumentRange | null {
  if (sourceContent === currentContent) {
    return range;
  }

  let prefixLength = 0;
  const sharedLength = Math.min(sourceContent.length, currentContent.length);
  while (
    prefixLength < sharedLength &&
    sourceContent.charCodeAt(prefixLength) === currentContent.charCodeAt(prefixLength)
  ) {
    prefixLength += 1;
  }

  let sourceSuffixStart = sourceContent.length;
  let currentSuffixStart = currentContent.length;
  while (
    sourceSuffixStart > prefixLength &&
    currentSuffixStart > prefixLength &&
    sourceContent.charCodeAt(sourceSuffixStart - 1) ===
      currentContent.charCodeAt(currentSuffixStart - 1)
  ) {
    sourceSuffixStart -= 1;
    currentSuffixStart -= 1;
  }

  if (range.end <= prefixLength) {
    return range;
  }

  if (range.start >= sourceSuffixStart) {
    const delta = currentSuffixStart - sourceSuffixStart;
    return {
      start: range.start + delta,
      end: range.end + delta,
    };
  }

  return null;
}

export function findClosestMarkdownRange(
  content: string,
  markdown: string,
  preferredStart: number,
): MarkdownDocumentRange | null {
  if (markdown.length === 0) {
    return null;
  }

  let bestStart: number | null = null;
  let searchStart = 0;
  while (searchStart <= content.length) {
    const foundStart = content.indexOf(markdown, searchStart);
    if (foundStart === -1) {
      break;
    }

    if (
      bestStart === null ||
      Math.abs(foundStart - preferredStart) < Math.abs(bestStart - preferredStart)
    ) {
      bestStart = foundStart;
    }
    searchStart = foundStart + Math.max(markdown.length, 1);
  }

  return bestStart === null
    ? null
    : {
        start: bestStart,
        end: bestStart + markdown.length,
      };
}
