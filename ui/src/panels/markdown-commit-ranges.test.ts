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
});
