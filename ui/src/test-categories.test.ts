import { describe, expect, it } from "vitest";
describe("UI test categories", () => {
  it("runs the unit project without browser globals", () => {
    expect(typeof document).toBe("undefined");
    expect(typeof window).toBe("undefined");
  });
});
