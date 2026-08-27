// Pins bounded, order-independent equality for JSON-like UI payloads.
// Does not exercise transcript policy or React lifecycle.

import { describe, expect, it } from "vitest";

import { valuesHaveSameBoundedContent } from "./bounded-content-equality";

describe("valuesHaveSameBoundedContent", () => {
  it("ignores object key insertion order within its comparison budget", () => {
    expect(
      valuesHaveSameBoundedContent(
        { first: 1, nested: { alpha: "a", beta: "b" } },
        { nested: { beta: "b", alpha: "a" }, first: 1 },
      ),
    ).toBe(true);
  });

  it("fails open to changed content when the comparison budget is exhausted", () => {
    expect(
      valuesHaveSameBoundedContent(
        { output: "same large output" },
        { output: "same large output" },
        { maxStringCharacters: 4 },
      ),
    ).toBe(false);
  });

  it("terminates for matching cyclic structures", () => {
    const left: Record<string, unknown> = {};
    const right: Record<string, unknown> = {};
    left.self = left;
    right.self = right;

    expect(valuesHaveSameBoundedContent(left, right)).toBe(true);
  });
});
