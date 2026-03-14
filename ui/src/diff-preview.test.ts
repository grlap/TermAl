import { describe, expect, it } from "vitest";

import { buildDiffPreviewModel } from "./diff-preview";

describe("buildDiffPreviewModel", () => {
  it("builds a minimal preview from simple add/remove lines", () => {
    expect(buildDiffPreviewModel("-hello\n+goodbye", "edit")).toMatchObject({
      changeSummary: {
        addedLineCount: 0,
        changedLineCount: 1,
        removedLineCount: 0,
      },
      hasStructuredPreview: true,
      modifiedText: "goodbye",
      note: null,
      originalText: "hello",
    });
  });

  it("omits unchanged regions outside hunk boundaries", () => {
    expect(
      buildDiffPreviewModel(
        [
          "@@ -1,2 +1,2 @@",
          " alpha",
          "-beta",
          "+bravo",
          "@@ -10,2 +10,2 @@",
          " charlie",
          "-delta",
          "+echo",
        ].join("\n"),
        "edit",
      ),
    ).toMatchObject({
      changeSummary: {
        addedLineCount: 0,
        changedLineCount: 2,
        removedLineCount: 0,
      },
      hasStructuredPreview: true,
      modifiedText: ["alpha", "bravo", "...", "charlie", "echo"].join("\n"),
      note: "Preview reconstructed from the patch. Unchanged regions outside shown hunks are omitted.",
      originalText: ["alpha", "beta", "...", "charlie", "delta"].join("\n"),
    });
  });

  it("keeps the original side empty for file creations", () => {
    expect(
      buildDiffPreviewModel(
        [
          "@@ -0,0 +1,2 @@",
          "+first line",
          "+second line",
        ].join("\n"),
        "create",
      ),
    ).toMatchObject({
      changeSummary: {
        addedLineCount: 2,
        changedLineCount: 0,
        removedLineCount: 0,
      },
      hasStructuredPreview: true,
      modifiedText: "first line\nsecond line",
      note: "Preview reconstructed from the patch. Unchanged regions outside shown hunks are omitted.",
      originalText: "",
    });
  });

  it("separates changed lines from extra additions and removals", () => {
    expect(
      buildDiffPreviewModel(
        [
          "@@ -1,3 +1,4 @@",
          "-before",
          "+after",
          " shared",
          "+extra",
          " shared-again",
          "-deleted",
        ].join("\n"),
        "edit",
      ).changeSummary,
    ).toEqual({
      addedLineCount: 1,
      changedLineCount: 1,
      removedLineCount: 1,
    });
  });

  it("builds hunk rows with line numbers and inline change highlights", () => {
    const preview = buildDiffPreviewModel(
      [
        "@@ -4,2 +4,2 @@",
        "-const greeting = 'hello';",
        "+const greeting = 'hi';",
        " const punctuation = '!';",
      ].join("\n"),
      "edit",
    );

    expect(preview.hunks).toHaveLength(1);
    expect(preview.hunks[0].header).toBe("@@ -4,2 +4,2 @@");
    expect(preview.hunks[0].rows[0]).toMatchObject({
      kind: "changed",
      left: {
        lineNumber: 4,
        text: "const greeting = 'hello';",
      },
      right: {
        lineNumber: 4,
        text: "const greeting = 'hi';",
      },
    });
    expect(preview.hunks[0].rows[0].left.highlights.length).toBeGreaterThan(0);
    expect(preview.hunks[0].rows[0].right.highlights.length).toBeGreaterThan(0);
    expect(preview.hunks[0].rows[1]).toMatchObject({
      kind: "context",
      left: {
        lineNumber: 5,
        text: "const punctuation = '!';",
      },
      right: {
        lineNumber: 5,
        text: "const punctuation = '!';",
      },
    });
  });
});
