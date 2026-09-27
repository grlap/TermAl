import { expect, it } from "vitest";
import { WaitDeltaWatermark } from "./wait-delta-watermark";

it("preserves same-commit siblings while rejecting stale creation and snapshots", () => {
  const watermark = new WaitDeltaWatermark();
  expect(watermark.snapshotCovers(1, "a")).toBe(true);
  expect(watermark.acceptDelta(5, "a", 1, false)).toBe(true);
  expect(watermark.acceptDelta(4, "a", 1, true)).toBe(false);
  expect(watermark.acceptDelta(5, "a", 1, true)).toBe(true);
  expect(watermark.snapshotCovers(4, "a")).toBe(false);
  expect(watermark.snapshotCovers(5, "a")).toBe(true);
  expect(watermark.acceptDelta(6, "a", 7, true)).toBe(false);
  expect(watermark.snapshotCovers(7, "a")).toBe(true);
});

it("uses a replacement server's revision space after its snapshot is adopted", () => {
  const watermark = new WaitDeltaWatermark();
  watermark.acceptDelta(100, "a", 1, false);
  expect(watermark.snapshotCovers(1, "b")).toBe(true);
  expect(watermark.acceptDelta(3, "b", 1, true)).toBe(true);
  expect(watermark.snapshotCovers(2, "b")).toBe(false);
  expect(watermark.snapshotCovers(3, "b")).toBe(true);
});

it("records consumption below the state revision even without a previous watermark", () => {
  const watermark = new WaitDeltaWatermark();
  expect(watermark.acceptDelta(5, "a", 10, false)).toBe(true);
  expect(watermark.snapshotCovers(4, "a")).toBe(false);
  expect(watermark.snapshotCovers(5, "a")).toBe(true);
});

it.each([true, false])("accepts a delta before the state revision is known (created=%s)", created => {
  const watermark = new WaitDeltaWatermark();
  expect(watermark.acceptDelta(5, "a", null, created)).toBe(true);
  expect(watermark.snapshotCovers(4, "a")).toBe(false);
});

it("does not reset the known server's watermark for a snapshot with empty identity", () => {
  const watermark = new WaitDeltaWatermark();
  watermark.acceptDelta(5, "a", 1, false);
  expect(watermark.snapshotCovers(4, "")).toBe(false);
  expect(watermark.snapshotCovers(5, "")).toBe(true);
  expect(watermark.snapshotCovers(4, "a")).toBe(false);
});
