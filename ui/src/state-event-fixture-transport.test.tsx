// Exercises the isolated transport through the production API and overview consumer.
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { fetchGitStatus, fetchSessionHistory, fetchSessionOverview, fetchSessionTail } from "./api";
import { projectConversationOverviewBuckets } from "./panels/conversation-overview-map";
import { installStateEventFixtureTransport } from "./dev/state-event-fixture-transport";
import { createStateEventWorkload } from "./dev/state-event-test-fixtures";

beforeEach(() => {
  // Track the original globals before the fixture installs its isolated boundary.
  vi.stubGlobal("fetch", globalThis.fetch);
  vi.stubGlobal("EventSource", globalThis.EventSource);
  vi.stubGlobal("SharedWorker", globalThis.SharedWorker);
});
afterEach(() => vi.unstubAllGlobals());

it("serves a complete bounded overview while the real API loads only a tail", async () => {
  const workload = createStateEventWorkload(2);
  const transport = installStateEventFixtureTransport(workload);
  const id = "synthetic-session-0";
  const tail = await fetchSessionTail(id, 20);
  expect(tail.session.messages).toHaveLength(20);
  expect(tail.session.messageCount).toBe(800);

  const overview = await fetchSessionOverview(id, 7);
  const projected = projectConversationOverviewBuckets(overview);
  expect(overview.sessionId).toBe(id);
  expect(overview.buckets).toHaveLength(7);
  expect(overview.buckets.reduce((count, bucket) => count + bucket.c, 0)).toBe(800);
  expect(overview.buckets.reduce((count, bucket) => count + bucket.u, 0)).toBe(400);
  expect(projected[0].startPosition).toBe(0);
  expect(projected[projected.length - 1].endPosition).toBe(800);
  expect(overview.latestPosition).toBe(799);
  expect(transport.refusedRequests).toEqual([]);
});

it("serves synthetic Git status and keeps unknown sessions and paths isolated", async () => {
  const transport = installStateEventFixtureTransport(createStateEventWorkload(2));
  const status = await fetchGitStatus("/synthetic", "synthetic-session-0");
  expect(status).toMatchObject({ workdir: "/synthetic", isClean: true, files: [] });
  await expect(fetchGitStatus("/unrelated", "synthetic-session-0")).rejects.toThrow();
  await expect(fetchSessionOverview("unknown-session")).rejects.toThrow();
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
