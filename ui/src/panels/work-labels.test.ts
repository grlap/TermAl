// Pure label membership, counts and group identity regressions; no tracker IO.
import { describe, expect, it } from "vitest";
import type { WorkItem } from "../work-visualizer-api";
import { groupWorkLabels, matchesWorkLabels, workLabelCounts } from "./work-labels";

const item = (id: string, labels: string[], source = "engram") => ({ id, labels, source } as WorkItem);

describe("loaded Work labels", () => {
  it("counts each label once per item, with cross-source identities kept separate", () => {
    const rows = [item("a", ["storage", "storage", "api"]), item("a", ["storage"], "beads"), item("b", [])];
    expect([...workLabelCounts(rows)]).toEqual([["storage", 2], ["api", 1]]);
    expect(groupWorkLabels(rows).map(group => [group.label, group.items.length])).toEqual([["api", 1], ["storage", 2], ["Unlabelled", 1]]);
  });

  it("matches exact source labels using Any/All and never infers labels from names", () => {
    const row = item("storage", ["a,b", "Docs", "<script>"]);
    expect(matchesWorkLabels(row, [], "all")).toBe(true);
    expect(matchesWorkLabels(row, ["a,b", "missing"], "any")).toBe(true);
    expect(matchesWorkLabels(row, ["a,b", "missing"], "all")).toBe(false);
    expect(matchesWorkLabels(row, ["a,b", "Docs"], "all")).toBe(true);
    expect(matchesWorkLabels(row, ["docs", "a", "storage"], "any")).toBe(false);
  });

  it("keeps incoming sort order inside groups and separates literal Unlabelled from the empty group", () => {
    const rows = [item("z", ["Unlabelled"]), item("a", []), item("b", ["Unlabelled"])];
    const groups = groupWorkLabels(rows);
    expect(groups.map(group => group.items.map(row => row.id))).toEqual([["z", "b"], ["a"]]);
    expect(new Set(groups.map(group => group.key)).size).toBe(2);
    expect(groupWorkLabels([])).toEqual([]);
  });
});
