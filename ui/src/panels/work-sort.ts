// New Work row ordering. Owns the one sort model that the table headers and
// the "Sort (loaded rows)" control share, and the comparison per column. Does
// not own filtering, the loaded rows, source pagination or any view state.
// Split out of WorkPanel.tsx when the table headers became sortable; the
// panel's inline priority/title sort moved here and gained the remaining
// columns and a direction (not a pure move).
import type { WorkItem } from "../work-visualizer-api";

export type WorkSortKey = "priority" | "title" | "source" | "kind" | "lifecycle" | "availability" | "waitsFor" | "assignment" | "updated";
export type WorkSortDirection = "asc" | "desc";
export type WorkSort = { key: WorkSortKey; direction: WorkSortDirection };

// The table's column order; the sort control lists the same columns.
export const WORK_SORT_COLUMNS: readonly { key: WorkSortKey; label: string }[] = [
  { key: "priority", label: "Priority" }, { key: "title", label: "Reference / title" }, { key: "source", label: "Source" },
  { key: "kind", label: "Kind" }, { key: "lifecycle", label: "Lifecycle" }, { key: "availability", label: "Availability" },
  { key: "waitsFor", label: "Waits for" }, { key: "assignment", label: "Assignment" }, { key: "updated", label: "Updated" },
];

// Dates read newest-first; every other column starts ascending (P0 first).
export function defaultWorkSortDirection(key: WorkSortKey): WorkSortDirection {
  return key === "updated" ? "desc" : "asc";
}

// Choosing the sorted column again reverses it; choosing another column
// starts that column at its default direction.
export function nextWorkSort(current: WorkSort | null, key: WorkSortKey): WorkSort {
  if (current?.key === key) return { key, direction: current.direction === "asc" ? "desc" : "asc" };
  return { key, direction: defaultWorkSortDirection(key) };
}

function sortValue(row: WorkItem, key: WorkSortKey): string | number | null {
  switch (key) {
    case "priority": return row.priority;
    case "title": return row.title;
    case "source": return row.source;
    case "kind": return row.kind;
    case "lifecycle": return row.lifecycle;
    case "availability": return row.availability;
    // What the row still waits for; satisfied prerequisites do not count.
    case "waitsFor": return row.prerequisites.filter(prerequisite => !prerequisite.satisfied).length;
    case "assignment": return row.assignedTo;
    case "updated": return row.updatedAt;
  }
}

// Rows the sort does not distinguish keep one order (id, then source) in both
// directions, so reversing a sort never shuffles equal rows.
function tie(a: WorkItem, b: WorkItem): number {
  return a.id.localeCompare(b.id) || a.source.localeCompare(b.source);
}

// Sorts a copy of the loaded rows; `null` keeps the source order (the Engram
// page, then the Beads snapshot). Rows without a value (unassigned) come
// last in both directions.
export function sortWorkRows(rows: readonly WorkItem[], sort: WorkSort | null): WorkItem[] {
  const copy = [...rows];
  if (!sort) return copy;
  const sign = sort.direction === "asc" ? 1 : -1;
  return copy.sort((a, b) => {
    const left = sortValue(a, sort.key);
    const right = sortValue(b, sort.key);
    if (left === null || right === null) return left === right ? tie(a, b) : left === null ? 1 : -1;
    const order = typeof left === "number" && typeof right === "number" ? left - right : String(left).localeCompare(String(right));
    return sign * order || tie(a, b);
  });
}
