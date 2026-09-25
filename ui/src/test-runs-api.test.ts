import { afterEach, expect, it, vi } from "vitest";
import { request } from "./api-request";
import { readTestRun, readTestRunLog, readTestRuns } from "./test-runs-api";
vi.mock("./api-request", () => ({ request: vi.fn().mockResolvedValue({}) }));
afterEach(() => vi.clearAllMocks());
it("encodes identifiers, forwards cancellation and requests only a bounded GET tail", async () => {
  const signal = new AbortController().signal;
  await readTestRuns("p /", signal);
  await readTestRun("run /", signal);
  await readTestRunLog("run /", "stage /", signal);
  expect(request).toHaveBeenNthCalledWith(1, "/api/test-runs?projectId=p%20%2F", { signal }, { preserveGatewayErrorBody: true });
  expect(request).toHaveBeenNthCalledWith(2, "/api/test-runs/run%20%2F", { signal }, { preserveGatewayErrorBody: true });
  expect(request).toHaveBeenNthCalledWith(3, "/api/test-runs/run%20%2F/stages/stage%20%2F/log?tail=65536", { signal }, { preserveGatewayErrorBody: true });
});
