import { describe, expect, it } from "vitest";

import {
  applyMarkdownDocumentEolStyle,
  buildFullMarkdownDiffDocumentSegments,
  buildGreedyMarkdownLineAnchors,
  buildMarkdownLineDiffAnchors,
  detectMarkdownDocumentEolStyle,
  normalizeMarkdownDocumentLineEndings,
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

  it("keeps Markdown bullet indentation depth changes visible", () => {
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

    expect(segments.map((segment) => segment.kind)).toEqual(["normal", "removed", "added"]);
    expect(segments.find((segment) => segment.kind === "removed")?.markdown).toContain(
      "  - server-side Apple / Google purchase validation and subscription upsert",
    );
    expect(segments.find((segment) => segment.kind === "added")?.markdown).toContain(
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

  it("keeps an interior changed fenced code line with its whole block", () => {
    const segments = buildFullMarkdownDiffDocumentSegments(
      "# Sync\n\n```text\nFlutter UI\nold path\n```\n\nAfter\n",
      "# Sync\n\n```text\nFlutter UI\nnew path\n```\n\nAfter\n",
    );

    const removedBlock = segments.find((segment) => segment.kind === "removed");
    const addedBlock = segments.find((segment) => segment.kind === "added");

    expect(removedBlock?.markdown).toBe("```text\nFlutter UI\nold path\n```\n");
    expect(addedBlock?.markdown).toBe("```text\nFlutter UI\nnew path\n```\n");
    expect(
      segments.some(
        (segment) =>
          segment.kind === "normal" &&
          (segment.markdown.includes("```text") || segment.markdown.includes("Flutter UI")),
      ),
    ).toBe(false);
  });

  it("separates text added before a changed Mermaid fence from the fence replacement", () => {
    // Regression guard for the "typed text smeared into the Mermaid
    // 'added' block" bug. Before the opening-marker-anchor fix, every
    // anchor inside the fence was dropped (interior text differs between
    // sides), leaving the LCS with no way to pin the fence position —
    // so paragraphs inserted BEFORE the fence got lumped into the same
    // "added" segment as the new fence, rendering the typed text twice
    // (once in the normal position where the user typed it, once again
    // inside the green "added" block with the new diagram).
    const segments = buildFullMarkdownDiffDocumentSegments(
      [
        "# Mermaid Demo",
        "",
        "```mermaid",
        "flowchart TD",
        "  Start --> End",
        "```",
        "",
      ].join("\n"),
      [
        "# Mermaid Demo",
        "",
        "for claude and codex",
        "for claude and codex",
        "",
        "```mermaid",
        "flowchart TD",
        "  Start --> Stop",
        "```",
        "",
      ].join("\n"),
    );

    // There must be an added segment that contains the new paragraph
    // text but NOT the fence interior. The old behaviour put both in
    // the same added segment — the user's typed paragraph ended up
    // smeared into the fence's green/added block.
    const prefixAddedSegment = segments.find(
      (segment) =>
        segment.kind === "added" &&
        segment.markdown.includes("for claude and codex"),
    );
    expect(prefixAddedSegment).toBeDefined();
    expect(prefixAddedSegment?.markdown).not.toContain("```mermaid");
    expect(prefixAddedSegment?.markdown).not.toContain("flowchart TD");

    // The fence interior replacement is its own removed + added pair,
    // with no "for claude and codex" bleed.
    const fenceRemoved = segments.find(
      (segment) =>
        segment.kind === "removed" &&
        segment.markdown.includes("Start --> End"),
    );
    const fenceAdded = segments.find(
      (segment) =>
        segment.kind === "added" &&
        segment.markdown.includes("Start --> Stop"),
    );
    expect(fenceRemoved?.markdown).not.toContain("for claude and codex");
    expect(fenceAdded?.markdown).not.toContain("for claude and codex");
    // And both ends of the fence replacement carry the full atomic
    // fence block (opening + interior + closing), matching the
    // existing "changed fence kept as whole old/new block" semantic.
    expect(fenceRemoved?.markdown).toContain("```mermaid");
    expect(fenceAdded?.markdown).toContain("```mermaid");
    expect(fenceRemoved?.markdown.endsWith("```\n")).toBe(true);
    expect(fenceAdded?.markdown.endsWith("```\n")).toBe(true);

    // Segment order: the pure-add text must come BEFORE the paired
    // removed+added fence replacement. That order is what lets the
    // renderer's change-block grouping break at an added→removed
    // transition, so the typed text lands in its own green block and
    // the fence replacement is a separate red→green pair below it.
    const orderedKinds = segments.map((segment) => segment.kind);
    const prefixAddedIndex = segments.indexOf(prefixAddedSegment!);
    const fenceRemovedIndex = segments.indexOf(fenceRemoved!);
    const fenceAddedIndex = segments.indexOf(fenceAdded!);
    expect(prefixAddedIndex).toBeLessThan(fenceRemovedIndex);
    expect(fenceRemovedIndex).toBeLessThan(fenceAddedIndex);
    // No "removed" segment appears before the prefix added one.
    expect(orderedKinds.slice(0, prefixAddedIndex)).not.toContain("removed");
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

    const beforeSectionTwo = beforeShift.filter(
      (segment) =>
        segment.kind === "added" &&
        segment.isInAfterDocument &&
        segment.markdown === "Section two original.\n",
    );
    const afterSectionTwo = afterShift.filter(
      (segment) =>
        segment.kind === "added" &&
        segment.isInAfterDocument &&
        segment.markdown === "Section two original.\n",
    );

    expect(beforeSectionTwo).toHaveLength(1);
    expect(afterSectionTwo).toHaveLength(1);
    expect(beforeSectionTwo[0]?.id).toBe(afterSectionTwo[0]?.id);
  });

  it("keeps repeated downstream segment ids stable when upstream line counts shift", () => {
    const beforeShift = buildFullMarkdownDiffDocumentSegments(
      [
        "# Title",
        "",
        "Section one base.",
        "",
        "Bridge context.",
        "",
        "Repeated base.",
        "",
        "Middle context.",
        "",
        "Repeated base.",
        "",
      ].join("\n"),
      [
        "# Title",
        "",
        "Section one original.",
        "",
        "Bridge context.",
        "",
        "Repeated original.",
        "",
        "Middle context.",
        "",
        "Repeated original.",
        "",
      ].join("\n"),
    );
    const afterShift = buildFullMarkdownDiffDocumentSegments(
      [
        "# Title",
        "",
        "Section one base.",
        "",
        "Bridge context.",
        "",
        "Repeated base.",
        "",
        "Middle context.",
        "",
        "Repeated base.",
        "",
      ].join("\n"),
      [
        "# Title",
        "",
        "Section one revised.",
        "",
        "Extra line.",
        "",
        "Bridge context.",
        "",
        "Repeated original.",
        "",
        "Middle context.",
        "",
        "Repeated original.",
        "",
      ].join("\n"),
    );

    const beforeRepeatedSegments = beforeShift.filter(
      (segment) =>
        segment.kind === "added" &&
        segment.isInAfterDocument &&
        segment.markdown === "Repeated original.\n",
    );
    const afterRepeatedSegments = afterShift.filter(
      (segment) =>
        segment.kind === "added" &&
        segment.isInAfterDocument &&
        segment.markdown === "Repeated original.\n",
    );

    expect(beforeRepeatedSegments).toHaveLength(2);
    expect(afterRepeatedSegments).toHaveLength(2);
    expect(beforeRepeatedSegments[1]?.id).toBe(afterRepeatedSegments[1]?.id);
  });

  // Reproduces the mermaid-demo.md save-failure scenario: heading unchanged,
  // one line inside a code fence changed. The user edits the heading; the
  // commit resolver must be able to find the unchanged heading segment in
  // the current source content. If the heading gets merged into a bigger
  // segment that also spans the fence, the serializer -> resolver round
  // trip breaks.
  it("keeps an unchanged heading as its own segment when a fenced-block line is changed", () => {
    const before = [
      "# Mermaid Demo\n",
      "\n",
      "```mermaid\n",
      "flowchart TD\n",
      "  Start --> Edit\n",
      "  Edit --> Stop\n",
      "```\n",
    ].join("");
    const after = [
      "# Mermaid Demo\n",
      "\n",
      "```mermaid\n",
      "flowchart TD\n",
      "  Start --> Edit\n",
      "  Edit --> End\n",
      "```\n",
    ].join("");

    const segments = buildFullMarkdownDiffDocumentSegments(before, after);

    const headingSegment = segments.find(
      (segment) => segment.kind === "normal" && segment.markdown.includes("# Mermaid Demo"),
    );
    expect(headingSegment).toBeDefined();
    if (!headingSegment) {
      return;
    }

    // The heading segment must exactly match the leading content of the
    // after document at its recorded offsets. If the range does, the
    // first check in `resolveRenderedMarkdownCommitRange` (equivalent to
    // `markdownRangeMatches`) succeeds and the edit is applied.
    expect(after.slice(headingSegment.afterStartOffset, headingSegment.afterEndOffset)).toBe(
      headingSegment.markdown,
    );

    // The heading segment must NOT swallow the opening fence line,
    // because the user edits the heading in isolation; if the fence is
    // merged in, the DOM serializer produces a different structure than
    // the original markdown and the commit's nextMarkdown can no longer
    // be range-mapped back to the source.
    expect(headingSegment.markdown).not.toContain("```mermaid");
  });

  // Regression for the Windows CRLF save-failure scenario for rendered-
  // Markdown diff edits. When the on-disk file uses CRLF line endings
  // (common on Windows checkouts with `core.autocrlf=true`), the builder
  // used to record offsets into the raw CRLF content while
  // `segment.markdown` was LF-normalized. The commit resolver then
  // compared `sliceRaw(start, end)` against `segment.markdown` and they
  // differed by every `\r` character, all three resolver fallbacks failed,
  // and the user saw "Rendered Markdown edit could not be applied because
  // the document changed under that section" with no network request and
  // (before the saveError-text UX fix) no explanation of why.
  //
  // The builder now normalizes CRLF → LF at entry, so segment offsets and
  // `segment.markdown` live in the same representation. Callers that pass
  // raw CRLF content must slice against the normalized form — which is
  // what `handleRenderedMarkdownSectionCommits` now does via
  // `normalizeMarkdownDocumentLineEndings(sourceContent)` before resolving.
  it("records LF-normalized offsets even when inputs contain CRLF line endings", () => {
    const beforeCrlf = [
      "# Mermaid Demo\r\n",
      "\r\n",
      "```mermaid\r\n",
      "flowchart TD\r\n",
      "  Start --> Edit\r\n",
      "  Edit --> Stop\r\n",
      "```\r\n",
    ].join("");
    const afterCrlf = [
      "# Mermaid Demo\r\n",
      "\r\n",
      "```mermaid\r\n",
      "flowchart TD\r\n",
      "  Start --> Edit\r\n",
      "  Edit --> End\r\n",
      "```\r\n",
    ].join("");

    const segments = buildFullMarkdownDiffDocumentSegments(beforeCrlf, afterCrlf);
    const headingSegment = segments.find(
      (segment) => segment.kind === "normal" && segment.markdown.includes("# Mermaid Demo"),
    );
    expect(headingSegment).toBeDefined();
    if (!headingSegment) {
      return;
    }

    // Under the fix, segment offsets point into the LF-normalized form of
    // the after document. A caller that normalizes its sourceContent the
    // same way can slice at those offsets and match segment.markdown
    // exactly — which is what the commit resolver needs.
    const normalizedAfter = normalizeMarkdownDocumentLineEndings(afterCrlf);
    const sliced = normalizedAfter.slice(
      headingSegment.afterStartOffset,
      headingSegment.afterEndOffset,
    );
    expect(sliced).toBe(headingSegment.markdown);
    // And segment.markdown itself must be LF, not raw CRLF.
    expect(headingSegment.markdown).not.toContain("\r");
  });
});

describe("detectMarkdownDocumentEolStyle", () => {
  // Pins the EOL-detection contract used by the rendered-Markdown
  // commit pipeline. The commit handler captures the source's EOL
  // style BEFORE normalizing to LF for segment math, then re-applies
  // the detected style to `nextDocumentContent` before writing it back
  // to the edit buffer. Ties fall back to `lf` so Unix files and new
  // buffers stay on LF, and so a brand-new empty document has a
  // defined shape.
  it("returns `lf` for an empty document", () => {
    expect(detectMarkdownDocumentEolStyle("")).toBe("lf");
  });

  it("returns `lf` for a document with no line endings at all", () => {
    expect(detectMarkdownDocumentEolStyle("a single line")).toBe("lf");
  });

  it("returns `lf` for a pure-LF document", () => {
    expect(detectMarkdownDocumentEolStyle("line1\nline2\nline3\n")).toBe("lf");
  });

  it("returns `crlf` for a pure-CRLF document", () => {
    expect(detectMarkdownDocumentEolStyle("line1\r\nline2\r\nline3\r\n")).toBe("crlf");
  });

  it("picks the dominant style in a mixed document (CRLF wins)", () => {
    // Two CRLF line endings + one LF → CRLF is dominant.
    expect(
      detectMarkdownDocumentEolStyle("line1\r\nline2\r\nline3\nline4"),
    ).toBe("crlf");
  });

  it("picks the dominant style in a mixed document (LF wins)", () => {
    // One CRLF + two LF → LF is dominant.
    expect(
      detectMarkdownDocumentEolStyle("line1\r\nline2\nline3\nline4"),
    ).toBe("lf");
  });

  it("breaks ties in favour of `lf`", () => {
    // Exactly one CRLF and one LF — equal counts → LF default.
    expect(detectMarkdownDocumentEolStyle("line1\r\nline2\nline3")).toBe("lf");
  });

  it("ignores bare `\\r` (legacy Mac) when counting", () => {
    // A bare CR is not followed by LF; it's neither a CRLF nor an LF,
    // so it does not contribute to either counter. A single \n still
    // wins, producing `lf`. This matches the project policy that
    // CR-only files are coerced to LF by
    // `normalizeMarkdownDocumentLineEndings`.
    expect(detectMarkdownDocumentEolStyle("line1\rline2\n")).toBe("lf");
  });

  it("counts CRLF correctly when followed by another CR", () => {
    // `\r\n\r` — the first two chars are a CRLF, the trailing bare
    // `\r` is ignored. One CRLF, zero LF → CRLF wins.
    expect(detectMarkdownDocumentEolStyle("line1\r\n\r")).toBe("crlf");
  });
});

describe("applyMarkdownDocumentEolStyle", () => {
  // Pins the "re-apply original EOL" half of the round-trip used by
  // the rendered-Markdown commit pipeline. Input is always
  // LF-normalized (the segment math keeps internal state on LF); the
  // helper returns either the input unchanged (`lf`) or every `\n`
  // replaced with `\r\n` (`crlf`).
  it("returns LF input unchanged when the style is `lf`", () => {
    const input = "line1\nline2\nline3\n";
    expect(applyMarkdownDocumentEolStyle(input, "lf")).toBe(input);
  });

  it("is identity on an empty string for both styles", () => {
    expect(applyMarkdownDocumentEolStyle("", "lf")).toBe("");
    expect(applyMarkdownDocumentEolStyle("", "crlf")).toBe("");
  });

  it("converts every `\\n` to `\\r\\n` when the style is `crlf`", () => {
    expect(applyMarkdownDocumentEolStyle("line1\nline2\nline3\n", "crlf")).toBe(
      "line1\r\nline2\r\nline3\r\n",
    );
  });

  it("returns content with no newlines unchanged for both styles", () => {
    expect(applyMarkdownDocumentEolStyle("no newline here", "lf")).toBe(
      "no newline here",
    );
    expect(applyMarkdownDocumentEolStyle("no newline here", "crlf")).toBe(
      "no newline here",
    );
  });

  it("round-trips: detect → normalize → apply reproduces the original CRLF document", () => {
    // Load-bearing invariant for the rendered-Markdown commit path:
    // the composition `apply(normalize(x), detect(x)) === x` for any
    // well-formed CRLF or LF document. If this breaks, the commit
    // handler's CRLF preservation breaks in the same shape.
    const crlfDocument = "# Title\r\n\r\nBody line 1.\r\nBody line 2.\r\n";
    const normalized = normalizeMarkdownDocumentLineEndings(crlfDocument);
    const style = detectMarkdownDocumentEolStyle(crlfDocument);
    expect(applyMarkdownDocumentEolStyle(normalized, style)).toBe(crlfDocument);

    const lfDocument = "# Title\n\nBody line 1.\nBody line 2.\n";
    const lfNormalized = normalizeMarkdownDocumentLineEndings(lfDocument);
    const lfStyle = detectMarkdownDocumentEolStyle(lfDocument);
    expect(applyMarkdownDocumentEolStyle(lfNormalized, lfStyle)).toBe(lfDocument);
  });
});
