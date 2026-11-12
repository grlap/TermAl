// Owns global native-history extent and unloaded-position mapping behavior.
// Does not substitute for request/anchoring and real pane integration tests.
import { describe, expect, it } from "vitest";
import { historyScrollExtent, unloadedHistoryPosition, withHistoryExtent } from "./conversation-history-scroll-geometry";

describe("native history extent", () => {
  it("keeps zero-extent page layout unchanged and includes missing space in every page coordinate", () => {
    const resident = { tops: [0, 120, 350], totalHeight: 700 };
    expect(withHistoryExtent(resident)).toBe(resident);
    expect(withHistoryExtent(resident, 2_000, 3_000)).toEqual({ tops: [2_000, 2_120, 2_350], totalHeight: 5_700 });
    expect(resident).toEqual({ tops: [0, 120, 350], totalHeight: 700 });
  });
  it("does not add estimated space around a fully resident conversation", () => {
    const extent = historyScrollExtent(40, 0, 40, 8_000);
    expect(extent.prefix).toBe(0);
    expect(extent.suffix).toBe(0);
    expect(unloadedHistoryPosition(0, extent)).toBeNull();
    expect(unloadedHistoryPosition(7_999, extent)).toBeNull();
  });

  it("maps a drag into missing prefix positions without treating loaded cards as missing", () => {
    const extent = historyScrollExtent(1_000, 980, 20, 4_000);
    expect(unloadedHistoryPosition(4_000, extent)).toBe(20);
    expect(unloadedHistoryPosition(100_000, extent)).toBe(500);
    expect(unloadedHistoryPosition(extent.prefix + 1_000, extent)).toBeNull();
    expect(unloadedHistoryPosition(-10, extent)).toBe(0);
  });

  it("maps both directions around an interior window and caps requests at the actual tail", () => {
    const extent = historyScrollExtent(1_000, 400, 40, 8_000);
    expect(unloadedHistoryPosition(100, extent)).toBe(0);
    expect(unloadedHistoryPosition(extent.prefix + 100, extent)).toBeNull();
    expect(unloadedHistoryPosition(extent.prefix + 8_000 + 12_000, extent)).toBe(500);
    expect(unloadedHistoryPosition(1_000_000, extent)).toBe(999);
  });

  it("keeps very long histories within browser scroll extent limits", () => {
    const extent = historyScrollExtent(100_000_000, 99_999_980, 20, 4_000);
    expect(extent.prefix + extent.suffix).toBeLessThanOrEqual(8_000_001);
    expect(unloadedHistoryPosition(extent.prefix / 2, extent)).toBeGreaterThan(49_000_000);
    expect(unloadedHistoryPosition(extent.prefix / 2, extent)).toBeLessThan(51_000_000);
  });
});
