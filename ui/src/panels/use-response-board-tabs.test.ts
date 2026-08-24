import { describe, expect, it } from "vitest";

import type { ResponseBoardTab } from "../api";
import { mergeResponseBoardTabOrder } from "./use-response-board-tabs";

function tab(
  id: string,
  sortOrder: number,
  overrides: Partial<ResponseBoardTab> = {},
): ResponseBoardTab {
  return {
    id,
    name: id,
    kind: "custom",
    projectId: null,
    sortOrder,
    createdAt: "2026-08-24T12:00:00Z",
    placedCardCount: 0,
    ...overrides,
  };
}

describe("mergeResponseBoardTabOrder", () => {
  it("does not resurrect tabs that exist only in the ordered snapshot", () => {
    const currentTabs = [
      tab("alpha", 0, { name: "Current alpha" }),
      tab("beta", 1, { name: "Current beta" }),
    ];

    const merged = mergeResponseBoardTabOrder(currentTabs, [
      tab("removed", 0),
      tab("beta", 1, { name: "Stale beta" }),
      tab("alpha", 2, { name: "Stale alpha" }),
    ]);

    expect(merged.map((candidate) => candidate.id)).toEqual([
      "beta",
      "alpha",
    ]);
    expect(merged.map((candidate) => candidate.name)).toEqual([
      "Current beta",
      "Current alpha",
    ]);
  });

  it("keeps current-only tabs after ordered tabs using their current order", () => {
    const merged = mergeResponseBoardTabOrder(
      [tab("later", 8), tab("ordered", 7), tab("earlier", 2)],
      [tab("ordered", 0)],
    );

    expect(merged.map((candidate) => candidate.id)).toEqual([
      "ordered",
      "earlier",
      "later",
    ]);
    expect(merged.map((candidate) => candidate.sortOrder)).toEqual([0, 1, 2]);
  });

  it("returns an empty list for empty inputs", () => {
    expect(mergeResponseBoardTabOrder([], [])).toEqual([]);
  });

  it("preserves object identity when normalized order is unchanged", () => {
    const currentTabs = [tab("alpha", 0), tab("beta", 1)];

    const merged = mergeResponseBoardTabOrder(currentTabs, currentTabs);

    expect(merged[0]).toBe(currentTabs[0]);
    expect(merged[1]).toBe(currentTabs[1]);
  });
});
