import { describe, expect, it } from "vitest";
import type { WorkItem } from "../work-visualizer-api";
import { defaultWorkSortDirection, nextWorkSort, sortWorkRows } from "./work-sort";

function item(id: string, extra: Partial<WorkItem> = {}): WorkItem {
  return { id, shortRef: id, title: id, kind: "task", lifecycle: "open", availability: "ready", priority: 2, labels: [], assignedTo: null, parentId: null, updatedAt: "2026-09-10T00:00:00Z", blockedBy: [], source: "beads", prerequisites: [], ...extra };
}
const ids = (rows: WorkItem[]) => rows.map(row => row.id);

describe("work-sort", () => {
  it("starts a column at its default direction and reverses it when chosen again", () => {
    expect(nextWorkSort(null, "kind")).toEqual({ key: "kind", direction: "asc" });
    expect(nextWorkSort(null, "updated")).toEqual({ key: "updated", direction: "desc" });
    expect(nextWorkSort({ key: "kind", direction: "asc" }, "kind")).toEqual({ key: "kind", direction: "desc" });
    expect(nextWorkSort({ key: "kind", direction: "desc" }, "kind")).toEqual({ key: "kind", direction: "asc" });
    expect(nextWorkSort({ key: "updated", direction: "desc" }, "priority")).toEqual({ key: "priority", direction: "asc" });
    expect(defaultWorkSortDirection("title")).toBe("asc");
  });

  it("keeps the source order without a sort and never mutates the input", () => {
    const rows = [item("z", { priority: 0 }), item("a", { priority: 3 })];
    const sorted = sortWorkRows(rows, null);
    expect(ids(sorted)).toEqual(["z", "a"]);
    expect(sorted).not.toBe(rows);
    sortWorkRows(rows, { key: "priority", direction: "asc" });
    expect(ids(rows)).toEqual(["z", "a"]);
  });

  it("sorts priority numerically, breaks ties by id then source, and keeps ties in place when reversed", () => {
    const rows = [item("n", { priority: 3 }), item("b", { priority: 0, source: "engram" }), item("b", { priority: 0 }), item("a", { priority: 0 }), item("m", { priority: 10 })];
    const ascending = sortWorkRows(rows, { key: "priority", direction: "asc" });
    expect(ascending.map(row => `${row.source}:${row.id}`)).toEqual(["beads:a", "beads:b", "engram:b", "beads:n", "beads:m"]);
    const descending = sortWorkRows(rows, { key: "priority", direction: "desc" });
    expect(descending.map(row => `${row.source}:${row.id}`)).toEqual(["beads:m", "beads:n", "beads:a", "beads:b", "engram:b"]);
  });

  it("counts only unsatisfied prerequisites as waits-for and keeps unassigned rows last in both directions", () => {
    const rows = [
      item("two", { prerequisites: [{ id: "x", satisfied: false }, { id: "y", satisfied: false }], assignedTo: "bob" }),
      item("none", { assignedTo: null }),
      item("one-satisfied", { prerequisites: [{ id: "x", satisfied: false }, { id: "y", satisfied: true }], assignedTo: "alice" }),
    ];
    expect(ids(sortWorkRows(rows, { key: "waitsFor", direction: "asc" }))).toEqual(["none", "one-satisfied", "two"]);
    expect(ids(sortWorkRows(rows, { key: "waitsFor", direction: "desc" }))).toEqual(["two", "one-satisfied", "none"]);
    expect(ids(sortWorkRows(rows, { key: "assignment", direction: "asc" }))).toEqual(["one-satisfied", "two", "none"]);
    expect(ids(sortWorkRows(rows, { key: "assignment", direction: "desc" }))).toEqual(["two", "one-satisfied", "none"]);
  });

  it("reads updated newest-first by default and text columns case-aware ascending", () => {
    const rows = [item("old", { updatedAt: "2026-09-01T00:00:00Z", kind: "task" }), item("new", { updatedAt: "2026-09-14T00:00:00Z", kind: "Bug" }), item("mid", { updatedAt: "2026-09-07T00:00:00Z", kind: "bug" })];
    expect(ids(sortWorkRows(rows, nextWorkSort(null, "updated")))).toEqual(["new", "mid", "old"]);
    expect(ids(sortWorkRows(rows, { key: "kind", direction: "asc" }))).toEqual(["mid", "new", "old"]);
  });
});
