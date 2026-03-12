import { describe, expect, it } from "vitest";

import { buildDiffPreviewModel } from "./diff-preview";

describe("buildDiffPreviewModel", () => {
  it("builds a minimal preview from simple add/remove lines", () => {
    expect(buildDiffPreviewModel("-hello\n+goodbye", "edit")).toEqual({
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
      hasStructuredPreview: true,
      modifiedText: "first line\nsecond line",
      note: "Preview reconstructed from the patch. Unchanged regions outside shown hunks are omitted.",
      originalText: "",
    });
  });
});
