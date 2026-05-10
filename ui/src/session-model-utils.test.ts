import { describe, expect, it } from "vitest";

import { isDefaultModelPreference } from "./session-model-utils";

describe("isDefaultModelPreference", () => {
  it.each([
    ["empty string", "", true],
    ["whitespace", "   ", true],
    ["lowercase sentinel", "default", true],
    ["title-case sentinel", "Default", true],
    ["uppercase sentinel", "DEFAULT", true],
    ["padded sentinel", " default ", true],
    ["prefix match", "defaults", false],
    ["custom model", "my-model", false],
  ])("classifies %s", (_label, value, expected) => {
    expect(isDefaultModelPreference(value)).toBe(expected);
  });
});
