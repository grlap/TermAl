// Loaded-row label semantics shared by the picker and grouped view. Labels are
// exact, case-sensitive source values; group identities cannot collide with a
// real label named "Unlabelled". No fetching or dependency inference here.
import type { WorkItem } from "../work-visualizer-api";

export type WorkLabelMode = "any" | "all";
export const workLabels = (item: WorkItem) => [...new Set(item.labels)];

export function matchesWorkLabels(item: WorkItem, selected: readonly string[], mode: WorkLabelMode) {
  return selected.length === 0 || (mode === "all"
    ? selected.every(label => item.labels.includes(label))
    : selected.some(label => item.labels.includes(label)));
}

export function workLabelCounts(rows: readonly WorkItem[]) {
  const counts = new Map<string, number>();
  for (const row of rows) for (const label of workLabels(row)) counts.set(label, (counts.get(label) ?? 0) + 1);
  return counts;
}

export function groupWorkLabels(rows: readonly WorkItem[]) {
  const labels = new Map<string, WorkItem[]>();
  const unlabelled: WorkItem[] = [];
  for (const row of rows) {
    const values = workLabels(row);
    if (!values.length) unlabelled.push(row);
    for (const value of values) {
      const group = labels.get(value) ?? [];
      group.push(row);
      labels.set(value, group);
    }
  }
  return [
    ...[...labels].sort(([a], [b]) => a.localeCompare(b)).map(([label, items]) => ({ key: JSON.stringify([label]), label, items })),
    ...(unlabelled.length ? [{ key: "unlabelled", label: "Unlabelled", items: unlabelled }] : []),
  ];
}
