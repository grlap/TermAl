// Owns: focused helper coverage for file-change message cards.
// Does not own: MessageCard dispatch, source opening side effects, or clipboard behavior.
// Split from: ui/src/MessageCard.test.tsx.

import { describe, expect, it } from "vitest";

import { fileChangeKindLabel } from "./file-changes-card";

describe("fileChangeKindLabel", () => {
  it("maps file change kinds to compact labels", () => {
    expect(fileChangeKindLabel("created")).toBe("A");
    expect(fileChangeKindLabel("modified")).toBe("M");
    expect(fileChangeKindLabel("deleted")).toBe("D");
    expect(fileChangeKindLabel("other")).toBe("*");
  });
});
