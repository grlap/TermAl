// Owns: focused regression coverage for session transcript resize policy.
// Does not own: ResizeObserver wiring, tail-follow state, or DOM scroll writes.

import { describe, expect, it } from "vitest";

import {
  resolveSessionPaneResizeMeasurement,
  shouldRepinSessionPaneAfterResize,
} from "./SessionPaneView.resize-measurement";

describe("session pane resize measurement", () => {
  it("ignores idle two-pixel content jitter but keeps live resize sensitivity", () => {
    expect(
      shouldRepinSessionPaneAfterResize({
        activePageChanged: false,
        contentHeightDelta: 2,
        shouldRepinEveryMeasuredPixel: false,
        viewportHeightDelta: 0,
      }),
    ).toBe(false);
    expect(
      shouldRepinSessionPaneAfterResize({
        activePageChanged: false,
        contentHeightDelta: 2,
        shouldRepinEveryMeasuredPixel: true,
        viewportHeightDelta: 0,
      }),
    ).toBe(true);
    expect(
      shouldRepinSessionPaneAfterResize({
        activePageChanged: false,
        contentHeightDelta: 2.1,
        shouldRepinEveryMeasuredPixel: false,
        viewportHeightDelta: 0,
      }),
    ).toBe(true);
  });

  it("accumulates same-direction idle refinements against the last handled height", () => {
    const firstMeasurement = resolveSessionPaneResizeMeasurement({
      activePageChanged: false,
      nextContentHeight: 102,
      nextViewportHeight: 500,
      previousContentHeight: 100,
      previousViewportHeight: 500,
      shouldRepinEveryMeasuredPixel: false,
    });
    expect(firstMeasurement).toEqual({
      nextContentHeightBaseline: 100,
      nextViewportHeightBaseline: 500,
      shouldRepin: false,
    });

    expect(
      resolveSessionPaneResizeMeasurement({
        activePageChanged: false,
        nextContentHeight: 104,
        nextViewportHeight: 500,
        previousContentHeight: firstMeasurement.nextContentHeightBaseline,
        previousViewportHeight: firstMeasurement.nextViewportHeightBaseline,
        shouldRepinEveryMeasuredPixel: false,
      }),
    ).toEqual({
      nextContentHeightBaseline: 104,
      nextViewportHeightBaseline: 500,
      shouldRepin: true,
    });
  });

  it.each([
    {
      label: "active page replacement",
      options: {
        activePageChanged: true,
        nextContentHeight: 100,
        nextViewportHeight: 500,
        previousContentHeight: 100,
        previousViewportHeight: 500,
        shouldRepinEveryMeasuredPixel: false,
      },
    },
    {
      label: "viewport-only change",
      options: {
        activePageChanged: false,
        nextContentHeight: 100,
        nextViewportHeight: 501,
        previousContentHeight: 100,
        previousViewportHeight: 500,
        shouldRepinEveryMeasuredPixel: false,
      },
    },
  ])("repins and advances both baselines for $label", ({ options }) => {
    expect(resolveSessionPaneResizeMeasurement(options)).toEqual({
      nextContentHeightBaseline: options.nextContentHeight,
      nextViewportHeightBaseline: options.nextViewportHeight,
      shouldRepin: true,
    });
  });

  it("lets live flow consume a refinement previously suppressed while idle", () => {
    const idleMeasurement = resolveSessionPaneResizeMeasurement({
      activePageChanged: false,
      nextContentHeight: 102,
      nextViewportHeight: 500,
      previousContentHeight: 100,
      previousViewportHeight: 500,
      shouldRepinEveryMeasuredPixel: false,
    });

    expect(
      resolveSessionPaneResizeMeasurement({
        activePageChanged: false,
        nextContentHeight: 102,
        nextViewportHeight: 500,
        previousContentHeight: idleMeasurement.nextContentHeightBaseline,
        previousViewportHeight: idleMeasurement.nextViewportHeightBaseline,
        shouldRepinEveryMeasuredPixel: true,
      }),
    ).toEqual({
      nextContentHeightBaseline: 102,
      nextViewportHeightBaseline: 500,
      shouldRepin: true,
    });
  });
});
