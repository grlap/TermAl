// Owns focused tests for the viewport-growth bottom-clamp classifier.
// Does not own scroll event wiring or indicator behaviour; those are covered
// by the pane and virtualized-list scroll tests that consume the classifier.

import { describe, expect, it } from "vitest";

import { isViewportGrowthBottomClamp } from "./message-stack-viewport-clamp";

describe("isViewportGrowthBottomClamp", () => {
  it("recognizes the browser clamp when the viewport grows under a bottom reader", () => {
    expect(
      isViewportGrowthBottomClamp({
        previousScrollTop: 9_400,
        previousScrollHeight: 10_000,
        previousClientHeight: 600,
        currentScrollTop: 9_280,
        currentScrollHeight: 10_000,
        currentClientHeight: 720,
      }),
    ).toBe(true);
  });

  it("still recognizes the clamp when content grew in the same frame", () => {
    expect(
      isViewportGrowthBottomClamp({
        previousScrollTop: 9_400,
        previousScrollHeight: 10_000,
        previousClientHeight: 600,
        currentScrollTop: 9_320,
        currentScrollHeight: 10_040,
        currentClientHeight: 720,
      }),
    ).toBe(true);
  });

  it("does not match an upward user scroll across stable geometry", () => {
    expect(
      isViewportGrowthBottomClamp({
        previousScrollTop: 9_400,
        previousScrollHeight: 10_000,
        previousClientHeight: 600,
        currentScrollTop: 8_800,
        currentScrollHeight: 10_000,
        currentClientHeight: 600,
      }),
    ).toBe(false);
  });

  it("does not match when the reader ends away from the bottom", () => {
    expect(
      isViewportGrowthBottomClamp({
        previousScrollTop: 9_400,
        previousScrollHeight: 10_000,
        previousClientHeight: 600,
        currentScrollTop: 8_000,
        currentScrollHeight: 10_000,
        currentClientHeight: 720,
      }),
    ).toBe(false);
  });

  it("does not match a scrollTop drop larger than the viewport growth", () => {
    expect(
      isViewportGrowthBottomClamp({
        previousScrollTop: 9_400,
        previousScrollHeight: 10_000,
        previousClientHeight: 600,
        currentScrollTop: 9_000,
        currentScrollHeight: 9_720,
        currentClientHeight: 720,
      }),
    ).toBe(false);
  });

  it("does not match when the viewport did not grow", () => {
    expect(
      isViewportGrowthBottomClamp({
        previousScrollTop: 9_400,
        previousScrollHeight: 10_000,
        previousClientHeight: 600,
        currentScrollTop: 9_399,
        currentScrollHeight: 10_000,
        currentClientHeight: 600,
      }),
    ).toBe(false);
  });
});
