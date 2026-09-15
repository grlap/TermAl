// New read-only Work API boundary. Owns wire types and GET calls, not tracker
// mutations, host identity selection, UI state or source receipt execution.
import { request } from "./api-request";

export type WorkPrerequisite = { id: string; satisfied: boolean };
export type WorkItem = {
  id: string; shortRef: string; title: string; kind: string; lifecycle: string;
  availability: string; priority: number; labels: string[]; assignedTo: string | null;
  parentId: string | null; updatedAt: string; blockedBy: string[];
  // `source` names the tracker the row came from; `prerequisites` are the
  // blocking edges that source reported for the row (never inferred).
  source: string; prerequisites: WorkPrerequisite[];
};
export type WorkFilters = { search: string; label: string; availability: string };
export type WorkPage = { items: WorkItem[]; total: number; shownBefore: number; more: boolean; after: string | null; hint: string | null };
export type WorkListResponse = {
  sources: { source: string; state: string; message: string }[];
  readerId: string | null; observedAt: string;
  page: WorkPage | null;
  // Beads shares the row model but not the Engram reader/cursor contract:
  // one full read per list request, no continuation, explicit source errors.
  beads: WorkPage | null;
};
export type WorkDetailResponse = {
  status: { work: { shortRef: string; title: string; outcome: string; acceptance: string[]; acceptanceOmitted: number | null; lifecycle: string; kind: string; priority: number }; availability: string } | null;
  holder: string | null; heldUntil: string | null;
  notes: { locator: string; kind: string; family: string; statusOwner?: boolean | null; nonHolder?: boolean | null; refs?: string[] | null; summary: string | null; by: string | null; createdAt: string; bodyOmitted: boolean; summaryTruncated: boolean }[];
  notesWindow: { total: number; shown: number; newer: number; older: number; after: string | null; readCut: { projectPosition: number; observedAt: string; validUntilMs: number | null } };
};
export type WorkBeadsDetailResponse = {
  item: WorkItem; description: string; parent: string | null;
  dependencies: { id: string; title: string; status: string; priority: number; kind: string; dependencyType: string }[];
  // Relation records the source receipt carried but the host could not read;
  // with any, or with fewer records than declared, availability is unknown.
  dependenciesUnread: number;
  dependentCount: number;
  comments: { id: string; author: string | null; text: string; createdAt: string }[];
  commentCount: number; observedAt: string;
};

export function readProjectWork(projectId: string, filters: WorkFilters, signal: AbortSignal, continuation?: { after: string; readerId: string }) {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(filters)) if (value) params.set(key, value);
  if (continuation) { params.set("after", continuation.after); params.set("readerId", continuation.readerId); }
  return request<WorkListResponse>(`/api/projects/${encodeURIComponent(projectId)}/work?${params}`, { signal }, { preserveGatewayErrorBody: true });
}

export function readWorkDetail(projectId: string, workRef: string, readerId: string, signal: AbortSignal, after?: string) {
  const params = new URLSearchParams({ readerId });
  if (after) params.set("after", after);
  return request<WorkDetailResponse>(`/api/projects/${encodeURIComponent(projectId)}/work/engram/${encodeURIComponent(workRef)}?${params}`, { signal }, { preserveGatewayErrorBody: true });
}

export function readWorkBeadsDetail(projectId: string, issueId: string, signal: AbortSignal) {
  return request<WorkBeadsDetailResponse>(`/api/projects/${encodeURIComponent(projectId)}/work/beads/${encodeURIComponent(issueId)}`, { signal }, { preserveGatewayErrorBody: true });
}
