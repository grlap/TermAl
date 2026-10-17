// Work presentation predicates over retained list rows. These fields never
// become list query parameters or cause detail/notes reads.
import type { Project } from "../types";
import type { WorkFilters, WorkItem } from "../work-visualizer-api";

export function matchesWorkDisplayFilters(row: WorkItem, filters: WorkFilters) {
  const search = filters.search.trim().toLocaleLowerCase();
  return (!search || [row.id, row.shortRef, row.title, ...row.labels, row.assignedTo ?? ""]
    .some(value => value.toLocaleLowerCase().includes(search)))
    && (!filters.label || row.labels.includes(filters.label))
    && (!filters.availability || (row.lifecycle === "open" && row.availability === filters.availability));
}

// A source configuration change replaces the snapshot, while unrelated
// project state (name, evaluator defaults, etc.) keeps the existing read.
export function workSourceKey(project: Project) {
  return JSON.stringify([project.id, project.rootPath, project.remoteId ?? null,
    project.engram?.enabled ?? false, project.engram?.binaryPath ?? null, project.engram?.home ?? null,
    project.engramDeclared ?? false, project.engramOperatorDisabled ?? false]);
}
