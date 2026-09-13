// New read-only Work API boundary. Owns wire types and GET calls, not tracker
// mutations, host identity selection, UI state or source receipt execution.
import { request } from "./api-request";

export type WorkItem = {
  id: string; shortRef: string; title: string; kind: string; lifecycle: string;
  availability: string; priority: number; labels: string[]; assignedTo: string | null;
  parentId: string | null; updatedAt: string; blockedBy: string[];
};
export type WorkFilters = { search: string; label: string; availability: string };
export type WorkListResponse = {
  sources: { source: string; state: string; message: string }[];
  readerSessionId: string | null; observedAt: string;
  page: { items: WorkItem[]; total: number; shownBefore: number; more: boolean; after: string | null; hint: string | null } | null;
};
export type WorkDetailResponse = {
  status: { work: { shortRef: string; title: string; outcome: string; acceptance: string[]; acceptanceOmitted: number | null; lifecycle: string; kind: string; priority: number }; availability: string } | null;
  holder: string | null; heldUntil: string | null;
  notes: { locator: string; kind: string; family: string; statusOwner?: boolean | null; nonHolder?: boolean | null; refs?: string[] | null; summary: string | null; by: string | null; createdAt: string; bodyOmitted: boolean; summaryTruncated: boolean }[];
  notesWindow: { total: number; shown: number; newer: number; older: number; after: string | null; readCut: { projectPosition: number; observedAt: string; validUntilMs: number | null } };
};

export function readProjectWork(projectId: string, filters: WorkFilters, signal: AbortSignal, continuation?: { after: string; readerSessionId: string }) {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(filters)) if (value) params.set(key, value);
  if (continuation) { params.set("after", continuation.after); params.set("readerSessionId", continuation.readerSessionId); }
  return request<WorkListResponse>(`/api/projects/${encodeURIComponent(projectId)}/work?${params}`, { signal }, { preserveGatewayErrorBody: true });
}

export function readWorkDetail(projectId: string, workRef: string, readerSessionId: string, signal: AbortSignal, after?: string) {
  const params = new URLSearchParams({ readerSessionId });
  if (after) params.set("after", after);
  return request<WorkDetailResponse>(`/api/projects/${encodeURIComponent(projectId)}/work/engram/${encodeURIComponent(workRef)}?${params}`, { signal }, { preserveGatewayErrorBody: true });
}
