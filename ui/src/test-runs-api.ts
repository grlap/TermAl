// Bounded read-only endpoints. Never launches, retries or cancels tests.
import { request } from "./api-request";
import type { TestRunDetail, TestRunSummary } from "./test-runs";

export function readTestRuns(projectId?: string, signal?: AbortSignal) {
  const query = projectId ? `?projectId=${encodeURIComponent(projectId)}` : "";
  return request<{ runs: TestRunSummary[] }>(`/api/test-runs${query}`, { signal }, { preserveGatewayErrorBody: true });
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
