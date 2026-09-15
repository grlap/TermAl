import { describe, expect, it } from "vitest";
import type { WorkItem } from "../work-visualizer-api";
import { MAX_TREE_DEPTH, buildDependencyForest, buildHierarchyForest, type WorkTreeNode } from "./work-tree";

function item(id: string, extra: Partial<WorkItem> = {}): WorkItem {
  return { id, shortRef: id, title: id, kind: "task", lifecycle: "open", availability: "ready", priority: 2, labels: [], assignedTo: null, parentId: null, updatedAt: "today", blockedBy: [], source: "beads", prerequisites: [], ...extra };
}
function waits(id: string, on: string[], extra: Partial<WorkItem> = {}) {
  return item(id, { prerequisites: on.map(dep => ({ id: dep, satisfied: false })), ...extra });
}
function ids(nodes: WorkTreeNode[]): unknown[] {
  return nodes.map(node => (node.children.length || node.repeatedCount
    ? { id: node.item.id, ...(node.repeatedCount ? { repeated: node.repeatedPreview, repeatedCount: node.repeatedCount } : {}), ...(node.children.length ? { children: ids(node.children) } : {}) }
    : node.item.id));
}
function count(nodes: WorkTreeNode[]): number {
  return nodes.reduce((sum, node) => sum + 1 + count(node.children), 0);
}
function counts(node: WorkTreeNode | undefined) {
  return node ? [node.absentUnsatisfied, node.absentSatisfied, node.hiddenByFilter] : null;
}
function depth(nodes: WorkTreeNode[]): number {
  let deepest = 0;
  const stack: [WorkTreeNode, number][] = nodes.map(node => [node, 1]);
  while (stack.length) {
    const [node, level] = stack.pop()!;
    deepest = Math.max(deepest, level);
    for (const child of node.children) stack.push([child, level + 1]);
  }
  return deepest;
}
function chain(length: number, relation: "dependencies" | "hierarchy"): WorkItem[] {
  return Array.from({ length }, (_, index) => relation === "dependencies"
    ? waits(`c${index}`, index ? [`c${index - 1}`] : [])
    : item(`c${index}`, { parentId: index + 1 < length ? `c${index + 1}` : null }));
}

describe("work-tree", () => {
  it("expands a shared prerequisite once and counts later occurrences on the parent", () => {
    const forest = buildDependencyForest([waits("goal", ["a", "b"]), waits("a", ["c"]), waits("b", ["c"]), item("c")]);
    expect(ids(forest.roots)).toEqual([{ id: "goal", children: [{ id: "a", children: ["c"] }, { id: "b", repeated: ["c"], repeatedCount: 1 }] }]);
    expect(forest.loose).toEqual([]);
    // The repeated reference knows which item it repeats, and the forest
    // knows where that item's one row lives, so a renderer can reveal it.
    expect(forest.roots[0]!.children[1]!.repeatedItems).toEqual(["beads:c"]);
    expect(forest.placedAt.get("beads:c")).toBe("beads:goal/beads:a/beads:c");
  });

  it("stays linear on dense shared chains instead of exploding per path", () => {
    const rows: WorkItem[] = [];
    for (let index = 0; index < 24; index += 1) {
      const deps = [index - 1, index - 2].filter(value => value >= 0).map(value => `n${value}`);
      rows.push(waits(`n${index}`, deps));
    }
    const forest = buildDependencyForest(rows);
    expect(count(forest.roots) + count(forest.loose)).toBe(rows.length);
    expect(forest.roots.map(node => node.item.id)).toEqual(["n23"]);
  });

  it("keeps one node per row even when every row depends on every earlier row", () => {
    const rows: WorkItem[] = [];
    for (let index = 0; index < 300; index += 1) {
      rows.push(waits(`d${index}`, Array.from({ length: index }, (_, j) => `d${j}`)));
    }
    const forest = buildDependencyForest(rows);
    expect(count(forest.roots) + count(forest.loose)).toBe(rows.length);
    // Every row is expanded exactly once, directly under the goal in source
    // order; each then only counts what is already shown above it.
    const goal = forest.roots[0]!;
    expect(goal.item.id).toBe("d299");
    expect(goal.children).toHaveLength(299);
    expect(goal.repeatedCount).toBe(0);
    const last = goal.children[298]!;
    expect(last.item.id).toBe("d298");
    expect(last.children).toEqual([]);
    expect(last.repeatedCount).toBe(298);
    expect(last.repeatedPreview).toEqual(["d0", "d1", "d2"]);
  });

  it("bounds nesting depth on long chains and continues them as later roots", () => {
    const rows = chain(4000, "dependencies");
    const forest = buildDependencyForest(rows);
    expect(count(forest.roots) + count(forest.loose)).toBe(rows.length);
    expect(depth(forest.roots)).toBe(MAX_TREE_DEPTH + 1);
    expect(forest.roots).toHaveLength(Math.ceil(rows.length / (MAX_TREE_DEPTH + 1)));
    expect(forest.roots[0]!.item.id).toBe("c3999");
    let cut = forest.roots[0]!;
    while (cut.children.length) cut = cut.children[0]!;
    expect(cut.item.id).toBe("c3799");
    expect(cut.deeperCount).toBe(1);
    // The cut prerequisite continues as the very next root, not as 3,800
    // single-row roots swept up in source order.
    expect(forest.roots[1]!.item.id).toBe("c3798");
    expect(forest.roots[1]!.repeatedCount).toBe(0);

    const hierarchy = buildHierarchyForest(chain(4000, "hierarchy"));
    expect(count(hierarchy.roots)).toBe(4000);
    expect(depth(hierarchy.roots)).toBe(MAX_TREE_DEPTH + 1);

    // A continuation is placed before any later regular root could claim it:
    // here a second goal also waits for the cut prerequisite.
    const twoGoals = [...chain(MAX_TREE_DEPTH + 3, "dependencies"), waits("late-goal", [`c${MAX_TREE_DEPTH - 1}`])];
    const both = buildDependencyForest(twoGoals);
    expect(both.roots.map(node => node.item.id)).toEqual([`c${MAX_TREE_DEPTH + 2}`, "c1", "late-goal"]);
    expect(both.roots[2]!.repeatedCount).toBe(1);
  });

  it("reserves a depth-cut prerequisite so no shallower node nests it in place", () => {
    // g waits for the top of a chain that cuts c1 at the depth bound, and for
    // c1 directly. c1 must become the promised later root, not g's child.
    const top = `c${MAX_TREE_DEPTH + 1}`;
    // Rows are listed top-down so g reaches the chain before c1 (children
    // follow row order): the chain cuts c1 first, then g meets it directly.
    const forest = buildDependencyForest([...chain(MAX_TREE_DEPTH + 2, "dependencies").reverse(), waits("g", [top, "c1"])]);
    expect(forest.roots.map(node => node.item.id)).toEqual(["g", "c1"]);
    const [g, c1] = forest.roots as [WorkTreeNode, WorkTreeNode];
    expect(g.children.map(child => child.item.id)).toEqual([top]);
    expect([g.deeperCount, g.repeatedCount]).toEqual([1, 0]);
    let cut = g;
    while (cut.children.length) cut = cut.children[0]!;
    expect([cut.item.id, cut.deeperCount]).toEqual(["c2", 1]);
    expect(c1.children.map(child => child.item.id)).toEqual(["c0"]);
    expect(count(forest.roots) + count(forest.loose)).toBe(MAX_TREE_DEPTH + 3);
  });

  it("orders dependency children by the visible rows' order, not the receipt's", () => {
    const rows = [waits("goal", ["low", "high"], { priority: 2 }), item("low", { title: "Zulu", priority: 4 }), item("high", { title: "Alpha", priority: 0 })];
    const children = (sorted: WorkItem[]) => buildDependencyForest(sorted, rows).roots[0]!.children.map(node => node.item.id);
    expect(children(rows)).toEqual(["low", "high"]);
    expect(children([...rows].sort((a, b) => a.priority - b.priority))).toEqual(["high", "low"]);
    expect(children([...rows].sort((a, b) => a.title.localeCompare(b.title)))).toEqual(["high", "low"]);
  });

  it("keeps blocking cycles visible as roots", () => {
    const forest = buildDependencyForest([waits("x", ["y"]), waits("y", ["x"]), item("free")]);
    expect(ids(forest.roots)).toEqual([{ id: "x", children: [{ id: "y", repeated: ["x"], repeatedCount: 1 }] }]);
    expect(ids(forest.loose)).toEqual(["free"]);
  });

  it("separates unsatisfied, satisfied and filter-hidden prerequisites that are not visible", () => {
    const alone = item("alone", { prerequisites: [{ id: "missing-1", satisfied: false }, { id: "closed-1", satisfied: true }, { id: "closed-2", satisfied: true }] });
    const goal = waits("goal", ["dep", "missing-3", "hidden-bug"]);
    const hiddenBug = item("hidden-bug", { kind: "bug" });
    const universe = [alone, goal, item("dep"), hiddenBug];
    const forest = buildDependencyForest(universe.filter(row => row.kind !== "bug"), universe);
    expect(forest.loose.map(node => [node.item.id, ...counts(node)!])).toEqual([["alone", 1, 2, 0]]);
    expect(counts(forest.roots[0])).toEqual([1, 0, 1]);
    expect(forest.roots[0]?.children.map(child => child.item.id)).toEqual(["dep"]);
  });

  it("never nests rows across sources even when identifiers collide", () => {
    const forest = buildDependencyForest([waits("same", ["dep"], { source: "engram" }), item("dep", { source: "beads" })]);
    expect(forest.roots).toEqual([]);
    expect(forest.loose.map(node => [node.item.source, node.item.id, node.absentUnsatisfied])).toEqual([["engram", "same", 1], ["beads", "dep", 0]]);
  });

  it("builds the hierarchy from loaded parents and survives a parent cycle", () => {
    const forest = buildHierarchyForest([item("root"), item("root.1", { parentId: "root" }), item("orphan", { parentId: "not-loaded" }), item("p", { parentId: "q" }), item("q", { parentId: "p" })]);
    expect(ids(forest.roots)).toEqual([{ id: "root", children: ["root.1"] }, "orphan", { id: "p", children: [{ id: "q", repeated: ["p"], repeatedCount: 1 }] }]);
    // A parent that is not loaded is silent; one hidden by a loaded-row
    // filter is disclosed on the child it leaves as a root.
    expect(forest.roots.map(node => node.hiddenByFilter)).toEqual([0, 0, 0]);
    const universe = [item("root", { kind: "epic" }), item("root.1", { parentId: "root" })];
    const filtered = buildHierarchyForest(universe.filter(row => row.kind !== "epic"), universe);
    expect(filtered.roots.map(node => [node.item.id, node.hiddenByFilter])).toEqual([["root.1", 1]]);
  });
});
