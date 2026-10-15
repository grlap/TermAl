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
  expect(request).toHaveBeenNthCalledWith(1, "/api/test-runs?limit=100&projectId=p%20%2F", { signal }, { preserveGatewayErrorBody: true });
  expect(request).toHaveBeenNthCalledWith(2, "/api/test-runs/run%20%2F", { signal }, { preserveGatewayErrorBody: true });
  expect(request).toHaveBeenNthCalledWith(3, "/api/test-runs/run%20%2F/stages/stage%20%2F/log?tail=65536", { signal }, { preserveGatewayErrorBody: true });
});

it("requests one bounded page and forwards only an explicit continuation", async () => {
  vi.mocked(request).mockResolvedValueOnce({ runs: [], limit: 100, nextCursor: "next /+" });
  const first = await readTestRuns();
  expect(first.nextCursor).toBe("next /+");
  expect(request).toHaveBeenCalledTimes(1);
  expect(request).toHaveBeenNthCalledWith(1, "/api/test-runs?limit=100", { signal: undefined }, { preserveGatewayErrorBody: true });
  const signal = new AbortController().signal;
  await readTestRuns("p /", signal, first.nextCursor!);
  expect(request).toHaveBeenCalledTimes(2);
  expect(request).toHaveBeenNthCalledWith(2, "/api/test-runs?limit=100&projectId=p%20%2F&after=next%20%2F%2B", { signal }, { preserveGatewayErrorBody: true });
});
