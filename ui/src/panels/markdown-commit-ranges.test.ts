import { describe, expect, it } from "vitest";

import {
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
