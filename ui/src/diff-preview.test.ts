import { describe, expect, it } from "vitest";

import { buildDiffPreviewModel } from "./diff-preview";

describe("buildDiffPreviewModel", () => {
  it("builds a minimal preview from simple add/remove lines", () => {
    expect(buildDiffPreviewModel("-hello\n+goodbye", "edit")).toEqual({
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
    ).toEqual({
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
    ).toEqual({
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

  it("reconstructs the whole file when latest file content is available", () => {
    expect(
      buildDiffPreviewModel(
        ["@@ -2 +2 @@", "-before", "+bravo"].join("\n"),
        "edit",
        "alpha\nbravo\ncharlie\n",
      ),
    ).toEqual({
      changeSummary: {
        addedLineCount: 0,
        changedLineCount: 1,
        removedLineCount: 0,
      },
      hasStructuredPreview: true,
      modifiedText: ["alpha", "bravo", "charlie"].join("\n"),
      note: null,
      originalText: ["alpha", "before", "charlie"].join("\n"),
    });
  });
});
