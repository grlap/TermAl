// New pure forest builders for the Work views. Owns which loaded rows nest
// under which, from edges the sources reported. Does not fetch, filter, or
// infer relations: a prerequisite that is not loaded stays a count, and the
// dependency and hierarchy relations are never merged into one tree.
//
// Every row is expanded at most once per forest. A later occurrence (a shared
// prerequisite, or a cycle back-edge) is not a node at all: the parent keeps
// a count and a short id preview of what is "shown above". The forest
// therefore has at most one node per visible row, whatever the edge count.
import type { WorkItem } from "../work-visualizer-api";

/** How many repeated prerequisite ids a parent lists by name. */
export const REPEATED_PREREQUISITE_PREVIEW = 3;
/**
 * Deepest nesting either tree expands. Both the builder and the renderer
 * recurse once per level, so an unbounded chain (Engram continuations have no
 * cumulative limit) would overflow the stack; a chain longer than this is cut
 * here, disclosed on the node, and its remainder continues as later roots.
 */
export const MAX_TREE_DEPTH = 200;

export type WorkTreeNode = {
  key: string;
  item: WorkItem;
  /** Dependency forest: whether the source reported this prerequisite as satisfied. */
  satisfied?: boolean;
  /** Reported prerequisites that are still unsatisfied and not among the loaded rows. */
  absentUnsatisfied: number;
  /** Reported prerequisites the source marked satisfied but never loaded (e.g. closed Beads blockers). */
  absentSatisfied: number;
  /** Loaded relations hidden from this view by a loaded-row filter: prerequisites here, the parent in the hierarchy. */
  hiddenByFilter: number;
  /** Visible relations already expanded elsewhere in this forest; only counted here. */
  repeatedCount: number;
  /** The first few of those, by short reference. */
  repeatedPreview: string[];
  /** All of those, by item key, so the renderer can reveal their one canonical row. */
  repeatedItems: string[];
  /** Visible relations not expanded here because a chain reached MAX_TREE_DEPTH (here, or where they were first cut); they continue as roots. */
  deeperCount: number;
  children: WorkTreeNode[];
};

export type WorkForest = {
  roots: WorkTreeNode[];
  /** Dependency forest only: rows neither blocked by nor blocking another visible row. */
  loose: WorkTreeNode[];
  /** Where each item's one node lives (item key → node key path), for revealing repeated references. */
  placedAt: ReadonlyMap<string, string>;
};

type AbsentCounts = Pick<WorkTreeNode, "absentUnsatisfied" | "absentSatisfied" | "hiddenByFilter">;

export function workItemKey(item: Pick<WorkItem, "source" | "id">) {
  return `${item.source}:${item.id}`;
}

function indexRows(rows: readonly WorkItem[]) {
  const byKey = new Map<string, WorkItem>();
  for (const row of rows) byKey.set(workItemKey(row), row);
  return byKey;
}

// Goals on top, what they wait for nested. `rows` are the visible rows;
// `universe` is every loaded row, so a prerequisite hidden by a loaded-row
// filter is disclosed as hidden rather than as not loaded. Roots are rows no
// visible row waits for; members of a blocking cycle have no such root and
// are promoted to roots in source order so they stay visible.
export function buildDependencyForest(rows: readonly WorkItem[], universe: readonly WorkItem[] = rows): WorkForest {
  const visible = indexRows(rows);
  const loaded = indexRows(universe);
  const lookup = (source: string, id: string) => visible.get(`${source}:${id}`) ?? null;
  const hasVisibleDependent = new Set<string>();
  for (const row of rows) for (const prerequisite of row.prerequisites) {
    const blocker = lookup(row.source, prerequisite.id);
    if (blocker) hasVisibleDependent.add(workItemKey(blocker));
  }
  const connected = new Set<string>();
  for (const row of rows) {
    const key = workItemKey(row);
    if (hasVisibleDependent.has(key) || row.prerequisites.some(p => lookup(row.source, p.id))) connected.add(key);
  }
  // Counts depend only on the item; compute each once.
  const countsByKey = new Map<string, AbsentCounts>();
  const countsFor = (item: WorkItem): AbsentCounts => {
    const key = workItemKey(item);
    const cached = countsByKey.get(key);
    if (cached) return cached;
    const counts = { absentUnsatisfied: 0, absentSatisfied: 0, hiddenByFilter: 0 };
    for (const prerequisite of item.prerequisites) {
      if (lookup(item.source, prerequisite.id)) continue;
      if (loaded.has(`${item.source}:${prerequisite.id}`)) counts.hiddenByFilter += 1;
      else if (prerequisite.satisfied) counts.absentSatisfied += 1;
      else counts.absentUnsatisfied += 1;
    }
    countsByKey.set(key, counts);
    return counts;
  };
  const placed = new Set<string>();
  const placedAt = new Map<string, string>();
  // Children follow the visible rows' order (the panel's sort), not the
  // receipt's prerequisite order, like the table and the hierarchy do.
  const rank = new Map(rows.map((row, index) => [workItemKey(row), index]));
  // Prerequisites a depth-cut node could not expand; each continues as a root
  // of its own, in the order the cuts happened. They are reserved from the
  // moment of the cut: a shallower node meeting one later in the same
  // traversal counts it instead of expanding it, so the promised root exists.
  const continuations: WorkItem[] = [];
  const continued = new Set<string>();
  const node = (item: WorkItem, path: string, depth: number, satisfied?: boolean): WorkTreeNode => {
    const key = workItemKey(item);
    const nodeKey = path ? `${path}/${key}` : key;
    placed.add(key);
    placedAt.set(key, nodeKey);
    const children: WorkTreeNode[] = [];
    let repeatedCount = 0;
    let deeperCount = 0;
    const repeatedPreview: string[] = [];
    const repeatedItems: string[] = [];
    const visiblePrerequisites = item.prerequisites
      .map(prerequisite => ({ prerequisite, blocker: lookup(item.source, prerequisite.id) }))
      .filter((entry): entry is { prerequisite: WorkItem["prerequisites"][number]; blocker: WorkItem } => entry.blocker !== null)
      .sort((a, b) => (rank.get(workItemKey(a.blocker)) ?? 0) - (rank.get(workItemKey(b.blocker)) ?? 0));
    for (const { prerequisite, blocker } of visiblePrerequisites) {
      const blockerKey = workItemKey(blocker);
      if (placed.has(blockerKey)) {
        repeatedCount += 1;
        repeatedItems.push(blockerKey);
        if (repeatedPreview.length < REPEATED_PREREQUISITE_PREVIEW) repeatedPreview.push(blocker.shortRef);
        continue;
      }
      if (continued.has(blockerKey)) { deeperCount += 1; continue; }
      if (depth >= MAX_TREE_DEPTH) { deeperCount += 1; continued.add(blockerKey); continuations.push(blocker); continue; }
      children.push(node(blocker, nodeKey, depth + 1, prerequisite.satisfied));
    }
    return { key: nodeKey, item, satisfied, ...countsFor(item), repeatedCount, repeatedPreview, repeatedItems, deeperCount, children };
  };
  const roots: WorkTreeNode[] = [];
  // A depth-cut chain continues as the very next roots, before any other
  // root could place its remainder somewhere else.
  const drainContinuations = () => {
    for (let next = continuations.shift(); next; next = continuations.shift()) {
      if (!placed.has(workItemKey(next))) roots.push(node(next, "", 0));
    }
  };
  for (const row of rows) {
    const key = workItemKey(row);
    if (connected.has(key) && !hasVisibleDependent.has(key) && !placed.has(key)) {
      roots.push(node(row, "", 0));
      drainContinuations();
    }
  }
  // Cycle members, in source order.
  for (const row of rows) {
    const key = workItemKey(row);
    if (connected.has(key) && !placed.has(key)) {
      roots.push(node(row, "", 0));
      drainContinuations();
    }
  }
  const loose = rows.filter(row => !connected.has(workItemKey(row))).map(row => node(row, "loose", 0));
  return { roots, loose, placedAt };
}

// Parent on top, subtasks nested. Roots are rows whose parent is not visible;
// a parent that is loaded but hidden by a loaded-row filter is disclosed on
// the root it leaves behind, and a parent cycle in the receipts is counted at
// the first repeated row.
export function buildHierarchyForest(rows: readonly WorkItem[], universe: readonly WorkItem[] = rows): WorkForest {
  const visible = indexRows(rows);
  const loaded = indexRows(universe);
  const childrenOf = new Map<string, WorkItem[]>();
  for (const row of rows) {
    if (!row.parentId) continue;
    const parentKey = `${row.source}:${row.parentId}`;
    if (!visible.has(parentKey)) continue;
    const siblings = childrenOf.get(parentKey);
    if (siblings) siblings.push(row); else childrenOf.set(parentKey, [row]);
  }
  const placed = new Set<string>();
  const placedAt = new Map<string, string>();
  const continuations: WorkItem[] = [];
  const continued = new Set<string>();
  const countsFor = (item: WorkItem): AbsentCounts => {
    const parentKey = item.parentId ? `${item.source}:${item.parentId}` : null;
    const parentHidden = parentKey !== null && !visible.has(parentKey) && loaded.has(parentKey);
    return { absentUnsatisfied: 0, absentSatisfied: 0, hiddenByFilter: parentHidden ? 1 : 0 };
  };
  const node = (item: WorkItem, path: string, depth: number): WorkTreeNode => {
    const key = workItemKey(item);
    const nodeKey = path ? `${path}/${key}` : key;
    placed.add(key);
    placedAt.set(key, nodeKey);
    const children: WorkTreeNode[] = [];
    let repeatedCount = 0;
    let deeperCount = 0;
    const repeatedPreview: string[] = [];
    const repeatedItems: string[] = [];
    for (const child of childrenOf.get(key) ?? []) {
      const childKey = workItemKey(child);
      if (placed.has(childKey)) {
        repeatedCount += 1;
        repeatedItems.push(childKey);
        if (repeatedPreview.length < REPEATED_PREREQUISITE_PREVIEW) repeatedPreview.push(child.shortRef);
        continue;
      }
      if (continued.has(childKey)) { deeperCount += 1; continue; }
      if (depth >= MAX_TREE_DEPTH) { deeperCount += 1; continued.add(childKey); continuations.push(child); continue; }
      children.push(node(child, nodeKey, depth + 1));
    }
    return { key: nodeKey, item, ...countsFor(item), repeatedCount, repeatedPreview, repeatedItems, deeperCount, children };
  };
  const roots: WorkTreeNode[] = [];
  const drainContinuations = () => {
    for (let next = continuations.shift(); next; next = continuations.shift()) {
      if (!placed.has(workItemKey(next))) roots.push(node(next, "", 0));
    }
  };
  for (const row of rows) {
    const key = workItemKey(row);
    if ((!row.parentId || !visible.has(`${row.source}:${row.parentId}`)) && !placed.has(key)) {
      roots.push(node(row, "", 0));
      drainContinuations();
    }
  }
  // Parent cycles, in source order.
  for (const row of rows) {
    if (!placed.has(workItemKey(row))) {
      roots.push(node(row, "", 0));
      drainContinuations();
    }
  }
  return { roots, loose: [], placedAt };
}
