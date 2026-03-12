import { describe, expect, it } from "vitest";

import { measurePaneTabStatusTooltipPosition } from "./pane-tab-status-tooltip";

describe("pane tab status tooltip placement", () => {
  it("keeps the tooltip inside the viewport when the anchor is near the right edge", () => {
    expect(
      measurePaneTabStatusTooltipPosition(
        {
          bottom: 56,
          left: 960,
          width: 120,
        },
        1024,
      ),
    ).toEqual({
      arrowLeft: 608 - 18,
      left: 404,
      top: 65,
      width: 608,
    });
  });

  it("aligns the tooltip to the viewport padding on narrow screens", () => {
    expect(
      measurePaneTabStatusTooltipPosition(
        {
          bottom: 48,
          left: 4,
          width: 72,
        },
        320,
      ),
    ).toEqual({
      arrowLeft: 28,
      left: 12,
      top: 57,
      width: 296,
    });
  });

  it("clamps the arrow away from the tooltip edges", () => {
    expect(
      measurePaneTabStatusTooltipPosition(
        {
          bottom: 40,
          left: 12,
          width: 12,
        },
        900,
      ).arrowLeft,
    ).toBe(18);
  });
});
