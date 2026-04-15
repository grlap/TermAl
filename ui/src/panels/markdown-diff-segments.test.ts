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
    expect(segments[0]?.markdown).not.toContain("\r");
  });

  it("treats Markdown prose indentation that renders the same as unchanged", () => {
    const segments = buildFullMarkdownDiffDocumentSegments(
      [
        "1. Reads are local.",
        "   Every screen should render from Drift streams or local queries.",
        "",
      ].join("\n"),
      [
        "1. Reads are local.",
        "Every screen should render from Drift streams or local queries.",
        "",
      ].join("\n"),
    );

    expect(segments.map((segment) => segment.kind)).toEqual(["normal"]);
    expect(segments[0]?.markdown).toContain(
      "Every screen should render from Drift streams or local queries.",
    );
  });

  it("treats Markdown bullet indentation that renders the same as unchanged", () => {
    const segments = buildFullMarkdownDiffDocumentSegments(
      [
        "- `validate-receipt`",
        "  - server-side Apple / Google purchase validation and subscription upsert",
        "",
      ].join("\n"),
      [
        "- `validate-receipt`",
        "- server-side Apple / Google purchase validation and subscription upsert",
        "",
      ].join("\n"),
    );

    expect(segments.map((segment) => segment.kind)).toEqual(["normal"]);
    expect(segments[0]?.markdown).toContain(
      "- server-side Apple / Google purchase validation and subscription upsert",
    );
  });

  it("keeps indentation changes in indented code blocks as changed", () => {
    const segments = buildFullMarkdownDiffDocumentSegments(
      "    const value = 1;\n",
      "const value = 1;\n",
    );

    expect(segments.map((segment) => segment.kind)).toEqual(["removed", "added"]);
  });

  it("keeps indentation changes inside fenced code blocks as changed", () => {
    const segments = buildFullMarkdownDiffDocumentSegments(
      "```\n  - server-side Apple / Google purchase validation\n```\n",
      "```\n- server-side Apple / Google purchase validation\n```\n",
    );

    expect(segments.map((segment) => segment.kind)).toContain("removed");
    expect(segments.map((segment) => segment.kind)).toContain("added");
  });

  it("keeps Markdown hard line breaks as changed", () => {
    const segments = buildFullMarkdownDiffDocumentSegments(
      "Line with hard break  \nNext line\n",
      "Line with hard break\nNext line\n",
    );

    expect(segments.map((segment) => segment.kind)).toEqual(["removed", "added", "normal"]);
  });

  it("normalizes matching CRLF documents without duplicating unchanged lines", () => {
    const segments = buildFullMarkdownDiffDocumentSegments(
      "# Title\r\n\r\nSame line\r\n",
      "# Title\r\n\r\nSame line\r\n",
    );

    expect(segments).toHaveLength(1);
    expect(segments[0]?.kind).toBe("normal");
    expect(segments[0]?.markdown).toBe("# Title\n\nSame line\n");
    expect(segments[0]?.markdown).not.toContain("\r");
  });

  it("keeps a changed fenced code opener with its whole block", () => {
    const segments = buildFullMarkdownDiffDocumentSegments(
      "# Sync\n\n```text\nFlutter UI\n\nRepository\n```\n\nAfter\n",
      "# Sync\n\n```\nFlutter UI\n\nRepository\n```\n\nAfter\n",
    );

    const removedBlock = segments.find((segment) => segment.kind === "removed");
    const addedBlock = segments.find((segment) => segment.kind === "added");

    expect(removedBlock?.markdown).toBe("```text\nFlutter UI\n\nRepository\n```\n");
    expect(addedBlock?.markdown).toBe("```\nFlutter UI\n\nRepository\n```\n");
    expect(
      segments.some(
        (segment) =>
          segment.kind === "normal" &&
          segment.markdown.includes("Flutter UI"),
      ),
    ).toBe(false);
  });

  it("keeps changed Mermaid fences as whole old and new diagram blocks", () => {
    const segments = buildFullMarkdownDiffDocumentSegments(
      [
        "# Mermaid Demo",
        "",
        "```mermaid",
        "flowchart TD",
        "  Start --> End",
        "```",
        "",
        "After",
        "",
      ].join("\n"),
      [
        "# Mermaid Demo",
        "",
        "```mermaid",
        "flowchart TD",
        "  Start --> Stop",
        "```",
        "",
        "After",
        "",
      ].join("\n"),
    );

    const removedBlock = segments.find((segment) => segment.kind === "removed");
    const addedBlock = segments.find((segment) => segment.kind === "added");

    expect(removedBlock?.markdown).toBe(
      ["```mermaid", "flowchart TD", "  Start --> End", "```", ""].join("\n"),
    );
    expect(addedBlock?.markdown).toBe(
      ["```mermaid", "flowchart TD", "  Start --> Stop", "```", ""].join("\n"),
    );
    expect(
      segments.some(
        (segment) =>
          segment.kind === "normal" &&
          segment.markdown.includes("flowchart TD"),
      ),
    ).toBe(false);
  });

  it("treats Markdown table separator formatting as unchanged", () => {
    const segments = buildFullMarkdownDiffDocumentSegments(
      [
        "| Error | Type | Retry? |",
        "|---------------------|------------------|--------|",
        "| socketException | network | yes |",
        "",
      ].join("\n"),
      [
        "| Error | Type | Retry? |",
        "| --- | --- | --- |",
        "| socketException | network | yes |",
        "",
      ].join("\n"),
    );

    expect(segments.map((segment) => segment.kind)).toEqual(["normal"]);
  });

  it("merges a reverted Markdown table row back into normal content", () => {
    const segments = buildFullMarkdownDiffDocumentSegments(
      [
        "| Error | Type | Retry? |",
        "|---------------------|------------------|--------|",
        "| socketException | network | yes |",
        "",
      ].join("\n"),
      [
        "| Error | Type | Retry? |",
        "| --- | --- | --- |",
        "| socketException | network | yes |",
        "",
      ].join("\n"),
    );

    expect(segments).toHaveLength(1);
    expect(segments[0]?.kind).toBe("normal");
    expect(segments[0]?.markdown).toContain("| socketException | network | yes |");
  });

  it("keeps unchanged downstream segment ids stable when upstream line counts shift", () => {
    const beforeShift = buildFullMarkdownDiffDocumentSegments(
      "# Title\n\nSection one base.\n\nSection two base.\n",
      "# Title\n\nSection one original.\n\nSection two original.\n",
    );
    const afterShift = buildFullMarkdownDiffDocumentSegments(
      "# Title\n\nSection one base.\n\nSection two base.\n",
      "# Title\n\nSection one revised.\n\nExtra line.\n\nSection two original.\n",
    );

    const beforeSectionTwo = beforeShift.find((segment) =>
      segment.markdown.includes("Section two original."),
    );
    const afterSectionTwo = afterShift.find((segment) =>
      segment.markdown.includes("Section two original."),
    );

    expect(beforeSectionTwo?.id).toBe(afterSectionTwo?.id);
  });
});
