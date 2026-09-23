import { describe, expect, it } from "vitest";

import {
  hasOverlappingMarkdownCommitRanges,
  resolveRenderedMarkdownCommitRange,
  type RenderedMarkdownSectionCommit,
} from "./markdown-commit-ranges";

describe("resolveRenderedMarkdownCommitRange", () => {
  it("maps a CRLF source baseline across an LF-normalized prefix change", () => {
    const sourceContent = "# Title\r\n\r\nSection one\r\n";
    const currentContent = "Intro\n# Title\n\nSection one\n";
    const markdown = "Section one\n";
    const rangeStart = "# Title\n\n".length;
    const commit: RenderedMarkdownSectionCommit = {
      allowCurrentSegmentFallback: false,
      currentSegment: {
        afterEndOffset: rangeStart + markdown.length,
        afterStartOffset: rangeStart,
        id: "segment-1",
        isInAfterDocument: true,
        kind: "normal",
        markdown,
        newStart: 3,
        oldStart: 3,
      },
      nextMarkdown: "Section one updated\n",
      segment: {
        afterEndOffset: rangeStart + markdown.length,
        afterStartOffset: rangeStart,
        id: "segment-1",
        isInAfterDocument: true,
        kind: "normal",
        markdown,
        newStart: 3,
        oldStart: 3,
      },
      sourceContent,
    };

    expect(resolveRenderedMarkdownCommitRange(currentContent, commit)).toEqual({
      start: "Intro\n".length + rangeStart,
      end: "Intro\n".length + rangeStart + markdown.length,
    });
  });

  it("uses the current-segment fallback only when the caller opts in", () => {
    const currentContent = "Intro\nCurrent section\nOutro\n";
    const currentStart = "Intro\n".length;
    const currentMarkdown = "Current section\n";
    const baseCommit: RenderedMarkdownSectionCommit = {
      allowCurrentSegmentFallback: false,
      currentSegment: {
        afterEndOffset: currentStart + currentMarkdown.length,
        afterStartOffset: currentStart,
        id: "segment-current",
        isInAfterDocument: true,
        kind: "normal",
        markdown: currentMarkdown,
        newStart: 2,
        oldStart: 2,
      },
      nextMarkdown: "Updated section\n",
      segment: {
        afterEndOffset: "Missing section\n".length,
        afterStartOffset: 0,
        id: "segment-old",
        isInAfterDocument: true,
        kind: "normal",
        markdown: "Missing section\n",
        newStart: 1,
        oldStart: 1,
      },
      sourceContent: "Missing section\n",
    };

    expect(resolveRenderedMarkdownCommitRange(currentContent, baseCommit)).toBeNull();
    expect(
      resolveRenderedMarkdownCommitRange(currentContent, {
        ...baseCommit,
        allowCurrentSegmentFallback: true,
      }),
    ).toEqual({
      start: currentStart,
      end: currentStart + currentMarkdown.length,
    });
  });
});

describe("hasOverlappingMarkdownCommitRanges", () => {
  // The real `RenderedMarkdownSectionCommit` carries segment objects that the
  // overlap helper never inspects. These tests only exercise the helper's
  // range arithmetic, so we cast a minimal stub instead of constructing a
  // full segment fixture.
  type OverlapCommit = Parameters<typeof hasOverlappingMarkdownCommitRanges>[0][number];
  const stubCommit = { commit: {} as OverlapCommit["commit"] };

  function rangeEntry(start: number, end: number): OverlapCommit {
    return { ...stubCommit, range: { start, end } };
  }

  it("returns false for strictly disjoint non-empty ranges", () => {
    expect(
      hasOverlappingMarkdownCommitRanges([rangeEntry(0, 5), rangeEntry(10, 20)]),
    ).toBe(false);
  });

  it("returns false for strictly adjacent non-empty ranges", () => {
    // `[5, 20)` and `[20, 25)` are both non-empty and share only the
    // boundary — the descending-by-start splice applies them
    // independently, so they must not be flagged as overlapping.
    expect(
      hasOverlappingMarkdownCommitRanges([rangeEntry(5, 20), rangeEntry(20, 25)]),
    ).toBe(false);
  });

  it("returns true when non-empty ranges overlap", () => {
    expect(
      hasOverlappingMarkdownCommitRanges([rangeEntry(0, 15), rangeEntry(10, 20)]),
    ).toBe(true);
  });

  it("returns true for two zero-length ranges sharing the same insertion point", () => {
    // Two rendered Markdown sections that both resolve to `[10, 10)`
    // would apply at the same offset in unspecified order and silently
    // garble the document. The overlap helper must reject the batch so
    // the user sees the save-error banner.
    expect(
      hasOverlappingMarkdownCommitRanges([rangeEntry(10, 10), rangeEntry(10, 10)]),
    ).toBe(true);
  });

  it("returns true when a zero-length range touches a non-empty sibling", () => {
    // `[5, 10)` ends at the same offset where a zero-length `[10, 10)`
    // sits. The descending splice would insert the zero-length write
    // into the replacement result of the first, not the original
    // source. Reject to avoid that surprise.
    expect(
      hasOverlappingMarkdownCommitRanges([rangeEntry(5, 10), rangeEntry(10, 10)]),
    ).toBe(true);
  });
});
