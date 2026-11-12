// Exercises the isolated transport through the production session/history APIs.
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { fetchGitStatus, fetchSessionHistory, fetchSessionTail } from "./api";
import { installStateEventFixtureTransport } from "./dev/state-event-fixture-transport";
import { createStateEventWorkload } from "./dev/state-event-test-fixtures";

beforeEach(() => {
  // Track the original globals before the fixture installs its isolated boundary.
  vi.stubGlobal("fetch", globalThis.fetch);
  vi.stubGlobal("EventSource", globalThis.EventSource);
  vi.stubGlobal("SharedWorker", globalThis.SharedWorker);
});
afterEach(() => vi.unstubAllGlobals());

it("serves synthetic Git status and keeps unknown sessions and paths isolated", async () => {
  const transport = installStateEventFixtureTransport(createStateEventWorkload(2));
  const status = await fetchGitStatus("/synthetic", "synthetic-session-0");
  expect(status).toMatchObject({ workdir: "/synthetic", isClean: true, files: [] });
  await expect(fetchGitStatus("/unrelated", "synthetic-session-0")).rejects.toThrow();
  await expect(fetchSessionTail("unknown-session", 20)).rejects.toThrow();
  expect(transport.refusedRequests).toHaveLength(2);
});

it("serves explicit history indices and refuses unsupported navigation through the real API", async () => {
  const transport = installStateEventFixtureTransport(createStateEventWorkload(2));
  const id = "synthetic-session-0";
  expect(await fetchSessionHistory(id, { start: 12, limit: 20 })).toMatchObject({ messageStartIndex: 12, hasMore: true, hasNewer: true });
  expect(await fetchSessionHistory(id, { limit: 20 })).toMatchObject({ messageStartIndex: 780, hasNewer: false });
  for (const options of [{ before: "cursor" }, { after: "cursor" }, { from: "start" as const }, { around: 300 }]) {
    await expect(fetchSessionHistory(id, { ...options, limit: 20 })).rejects.toMatchObject({
      kind: "backend-unavailable",
      message: "The TermAl backend is unavailable.",
      cause: expect.objectContaining({ message: expect.stringContaining("refused history navigation") }),
    });
    expect(transport.refusedRequests[transport.refusedRequests.length - 1]).toContain(`&${Object.keys(options)[0]}=`);
  }
  expect(transport.refusedRequests).toHaveLength(4);
});
