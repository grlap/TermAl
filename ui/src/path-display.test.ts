import { describe, expect, it } from "vitest";

import { normalizeDisplayPath, relativizePathToWorkspace } from "./path-display";

describe("path-display", () => {
  it("normalizes Windows extended-length paths for display", () => {
    expect(normalizeDisplayPath("\\\\?\\C:\\github\\Personal\\TermAl\\src\\runtime.rs")).toBe(
      "C:/github/Personal/TermAl/src/runtime.rs",
    );
  });

  it("relativizes file paths against Windows extended-length workspace roots", () => {
    expect(
      relativizePathToWorkspace(
        "C:\\github\\Personal\\TermAl\\src\\runtime.rs",
        "\\\\?\\C:\\github\\Personal\\TermAl",
      ),
    ).toBe("src/runtime.rs");
  });
});
