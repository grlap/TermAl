import { describe, expect, it } from "vitest";

import {
  buildFullMarkdownDiffDocumentSegments,
  buildGreedyMarkdownLineAnchors,
  buildMarkdownLineDiffAnchors,
  splitMarkdownDocumentLinesWithOffsets,
} from "./markdown-diff-segments";

describe("markdown diff segments", () => {
  it("builds no anchors for empty documents", () => {
    expect(buildMarkdownLineDiffAnchors([], [])).toEqual([]);
    expect(buildGreedyMarkdownLineAnchors([], [])).toEqual([]);
  });

  it("builds a single anchor for matching single lines", () => {
    const beforeLines = splitMarkdownDocumentLinesWithOffsets("same\n");
    const afterLines = splitMarkdownDocumentLinesWithOffsets("same\n");

    expect(buildMarkdownLineDiffAnchors(beforeLines, afterLines)).toEqual([
      { beforeIndex: 0, afterIndex: 0 },
    ]);
  });

  it("builds anchors above the LCS cell limit", () => {
    const beforeLines = splitMarkdownDocumentLinesWithOffsets(
      Array.from({ length: 1000 }, (_, index) => `line ${index}\n`).join(""),
    );
    const afterLines = splitMarkdownDocumentLinesWithOffsets(
      Array.from({ length: 1000 }, (_, index) => `line ${index}\n`).join(""),
    );

    const anchors = buildMarkdownLineDiffAnchors(beforeLines, afterLines);

    expect(anchors).toHaveLength(1000);
    expect(anchors[0]).toEqual({ beforeIndex: 0, afterIndex: 0 });
    expect(anchors[999]).toEqual({ beforeIndex: 999, afterIndex: 999 });
  });

  it("does not let repeated separators consume the tail of a large Markdown document", () => {
    const prefix = Array.from(
      { length: 360 },
      (_, index) => `## Existing ${index}\n\nStable body ${index}.\n\n---\n\n`,
    ).join("");
    const suffix = [
      "## Future Refactoring\n",
      "\n",
      "Stable refactoring notes.\n",
      "\n",
      "---\n",
      "\n",
      "## Issue Tracking Process\n",
      "\n",
      "Stable process notes.\n",
      "\n",
      "## Notes\n",
      "\n",
      "Stable tail notes.\n",
    ].join("");
    const inserted = Array.from(
      { length: 260 },
      (_, index) => `## Added ${index}\n\nNew body ${index}.\n\n---\n\n`,
    ).join("");

    const segments = buildFullMarkdownDiffDocumentSegments(
      `# Bugs\n\n${prefix}${suffix}`,
      `# Bugs\n\n${prefix}${inserted}${suffix}`,
    );

    expect(segments.find((segment) => segment.markdown.includes("## Added 0"))?.kind).toBe("added");
    expect(segments.find((segment) => segment.markdown.includes("## Future Refactoring"))?.kind).toBe("normal");
    expect(segments.find((segment) => segment.markdown.includes("## Issue Tracking Process"))?.kind).toBe("normal");
    expect(segments.find((segment) => segment.markdown.includes("## Notes"))?.kind).toBe("normal");
  });

  it("builds greedy anchors from the first forward match after the cursor", () => {
    const beforeLines = splitMarkdownDocumentLinesWithOffsets("alpha\nbeta\ngamma\nbeta\n");
    const afterLines = splitMarkdownDocumentLinesWithOffsets("beta\nalpha\nbeta\ngamma\n");

    expect(buildGreedyMarkdownLineAnchors(beforeLines, afterLines)).toEqual([
      { beforeIndex: 1, afterIndex: 0 },
      { beforeIndex: 3, afterIndex: 2 },
    ]);
  });

  it("renders link-only normalized matches as changed segments", () => {
    const segments = buildFullMarkdownDiffDocumentSegments(
      "See [the guide](guide.md).\n",
      "See the guide.\n",
    );

    expect(segments.map((segment) => segment.kind)).toEqual(["removed", "added"]);
    expect(segments[0]?.markdown).toBe("See [the guide](guide.md).\n");
    expect(segments[1]?.markdown).toBe("See the guide.\n");
  });

  it("does not mark CRLF-only matches as changed segments", () => {
    const segments = buildFullMarkdownDiffDocumentSegments("Same line\r\n", "Same line\n");

    expect(segments.map((segment) => segment.kind)).toEqual(["normal"]);
    expect(segments[0]?.markdown).toBe("Same line\n");
  });
});
