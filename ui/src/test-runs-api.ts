// Bounded read-only endpoints. Never launches, retries or cancels tests.
import { request } from "./api-request";
import type { TestRunDetail, TestRunSummary } from "./test-runs";

export interface TestRunPage {
  runs: TestRunSummary[];
  limit: number;
  nextCursor: string | null;
  revision?: number;
  serverInstanceId?: string;
  total?: number;
}
export interface TestRunFilters {
  sessionId?: string;
  state?: import("./test-runs").TestRunState | "";
  view?: "history" | "active" | "attention";
}
export function readTestRuns(projectId?: string, signal?: AbortSignal, after?: string, filters: TestRunFilters = {}) {
  const params = new URLSearchParams({ limit: "100" });
  if (projectId) params.set("projectId", projectId);
  if (after) params.set("after", after);
  if (filters.sessionId) params.set("sessionId", filters.sessionId);
  if (filters.state) params.set("state", filters.state);
  if (filters.view) params.set("view", filters.view);
  const query = `?${params.toString().replace(/\+/g, "%20")}`;
  return request<TestRunPage>(`/api/test-runs${query}`, { signal }, { preserveGatewayErrorBody: true });
}
export function readTestRun(runId: string, signal?: AbortSignal) {
  return request<{ run: TestRunSummary; detail: TestRunDetail }>(
    `/api/test-runs/${encodeURIComponent(runId)}`, { signal }, { preserveGatewayErrorBody: true },
  );
}
export function readTestRunLog(runId: string, stage: string, signal?: AbortSignal) {
  return request<{ text: string; truncated: boolean; size: number }>(
    `/api/test-runs/${encodeURIComponent(runId)}/stages/${encodeURIComponent(stage)}/log?tail=65536`,
    { signal }, { preserveGatewayErrorBody: true },
  );
}
