// Owns focused tests for the shared message-stack DOM scroll-write seam.
// Does not own pane intent, virtualizer reconciliation, or drag/drop policy.

import { describe, expect, it, vi } from "vitest";

import { writeMessageStackScrollTopImmediately } from "./message-stack-scroll-sync";

describe("writeMessageStackScrollTopImmediately", () => {
  it("aborts native smooth scrolling before publishing the synchronous landing", () => {
    const node = document.createElement("section");
    let currentTop = 900;
    const writes: string[] = [];
    Object.defineProperty(node, "scrollTop", {
      configurable: true,
      get: () => currentTop,
      set: (top: number) => {
        writes.push(`assign:${top}`);
        currentTop = top;
      },
    });
    node.scrollTo = vi.fn((optionsOrX?: ScrollToOptions | number, y?: number) => {
      const top =
        typeof optionsOrX === "object" && optionsOrX !== null
          ? optionsOrX.top
          : y;
      writes.push(`auto:${top}`);
    }) as typeof node.scrollTo;

    writeMessageStackScrollTopImmediately(node, 321);

    expect(node.scrollTo).toHaveBeenCalledWith({
      top: 321,
      behavior: "auto",
    });
    expect(writes).toEqual(["auto:321", "assign:321"]);
    expect(node.scrollTop).toBe(321);
  });
});
