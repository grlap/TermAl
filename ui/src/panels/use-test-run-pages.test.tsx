import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as api from "../test-runs-api";
import { makeTestRun } from "../test-runs-fixtures";
import { useTestRunPages } from "./use-test-run-pages";
import type { TestRunViewRevision } from "../test-run-overview";

vi.mock("../test-runs-api", () => ({ readTestRuns: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });
const revision = (version: number): TestRunViewRevision => ({ serverInstanceId: "server", revision: version, version });
const initial = revision(1);
const page = (ids: string[], version = 1, nextCursor: string | null = null): api.TestRunPage => ({
  runs: ids.map(runId => makeTestRun({ runId })), limit: 100, nextCursor, revision: version, serverInstanceId: "server",
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  return { promise: new Promise<T>(done => { resolve = done; }), resolve: (value: T) => resolve(value) };
}

it.each([false, true])("merges overlapping live pages by response revision and preserves a newer alias restoration (newer first: %s)", async newerFirst => {
  const read = vi.mocked(api.readTestRuns).mockResolvedValueOnce(page(["first"], 1, "second"))
    .mockResolvedValueOnce(page(["second"], 1));
  const hook = renderHook(({ rev }) => useTestRunPages("", { view: "active" }, rev), { initialProps: { rev: initial } });
  await waitFor(() => expect(hook.result.current.loaded).toBe(true));
  act(() => hook.result.current.more());
  await waitFor(() => expect(hook.result.current.runs).toHaveLength(2));
  act(() => hook.result.current.visible(["second"]));
  const newer = deferred<api.TestRunPage>();
  const older = deferred<api.TestRunPage>();
  read.mockReturnValueOnce(newer.promise).mockReturnValueOnce(older.promise);
  hook.rerender({ rev: revision(2) });
  await waitFor(() => expect(read).toHaveBeenCalledTimes(4));
  const newPage = { ...page([], 4, "second"), runs: [makeTestRun({ runId: "shared", currentStage: "new-stage" }), makeTestRun({ runId: "restored" })] };
  const oldPage = { ...page([], 3), runs: [makeTestRun({ runId: "shared", currentStage: "old-stage" }),
    makeTestRun({ runId: "old-canonical", supersededRunIds: ["restored"] })] };
  if (newerFirst) {
    await act(async () => newer.resolve(newPage));
    await act(async () => older.resolve(oldPage));
  } else {
    await act(async () => older.resolve(oldPage));
    await act(async () => newer.resolve(newPage));
  }
  expect(hook.result.current.runs.find(run => run.runId === "shared")?.currentStage).toBe("new-stage");
  expect(hook.result.current.runs.some(run => run.runId === "restored")).toBe(true);
  expect(read).toHaveBeenCalledTimes(4);
});

it("reissues an explicitly requested continuation after invalidation cancels its older request", async () => {
  const stale = deferred<api.TestRunPage>();
  const next = deferred<api.TestRunPage>();
  const read = vi.mocked(api.readTestRuns).mockResolvedValueOnce(page(["first"], 1, "older"))
    .mockReturnValueOnce(stale.promise).mockResolvedValueOnce(page(["first-new"], 2, "older"))
    .mockReturnValueOnce(next.promise);
  const hook = renderHook(({ rev }) => useTestRunPages("", {}, rev), { initialProps: { rev: initial } });
  await waitFor(() => expect(hook.result.current.loaded).toBe(true));
  act(() => hook.result.current.more());
  const oldSignal = read.mock.calls[1][1];
  hook.rerender({ rev: revision(2) });
  await waitFor(() => expect(read).toHaveBeenCalledTimes(4));
  expect(read.mock.calls.map(call => call[2])).toEqual([undefined, "older", undefined, "older"]);
  expect(oldSignal?.aborted).toBe(true);
  await act(async () => stale.resolve(page(["stale-page"])));
  await act(async () => next.resolve(page(["requested-older"], 2)));
  expect(hook.result.current.runs.map(run => run.runId)).toEqual(["first-new", "requested-older"]);
});

it.each([false, true])("reconnects loaded pages when a new run shifts the boundary (old continuation first: %s)", async oldFirst => {
  const read = vi.mocked(api.readTestRuns).mockResolvedValueOnce(page(["first", "boundary"], 1, "after-boundary"))
    .mockResolvedValueOnce(page(["older"], 1, "unread"));
  const hook = renderHook(({ rev }) => useTestRunPages("", { view: "active" }, rev), { initialProps: { rev: initial } });
  await waitFor(() => expect(hook.result.current.loaded).toBe(true));
  act(() => hook.result.current.more());
  await waitFor(() => expect(hook.result.current.runs).toHaveLength(3));
  act(() => hook.result.current.visible(["older"]));
  const first = deferred<api.TestRunPage>();
  const stale = deferred<api.TestRunPage>();
  read.mockReturnValueOnce(first.promise).mockReturnValueOnce(stale.promise)
    .mockResolvedValueOnce(page(["boundary", "older"], 2, "unread"));
  hook.rerender({ rev: revision(2) });
  await waitFor(() => expect(read).toHaveBeenCalledTimes(4));
  const staleSignal = read.mock.calls[3][1];
  if (oldFirst) await act(async () => stale.resolve(page(["older"], 2, "unread")));
  await act(async () => first.resolve(page(["newest", "first"], 2, "after-first")));
  await waitFor(() => expect(hook.result.current.runs).toHaveLength(4));
  expect(hook.result.current.runs.map(run => run.runId)).toEqual(expect.arrayContaining(["newest", "first", "boundary", "older"]));
  expect(read.mock.calls.map(call => call[2])).toEqual([undefined, "after-boundary", undefined, "after-boundary", "after-first"]);
  expect(staleSignal?.aborted).toBe(!oldFirst);
  await act(async () => stale.resolve(page(["obsolete"], 2, "wrong-continuation")));
  expect(hook.result.current.runs.some(run => run.runId === "obsolete")).toBe(false);
  expect(hook.result.current.hasMore).toBe(true);
  expect(read).toHaveBeenCalledTimes(5);
});

it("retains a manual continuation retry when an invalidation cancels its request", async () => {
  const retry = deferred<api.TestRunPage>();
  const refreshed = deferred<api.TestRunPage>();
  const read = vi.mocked(api.readTestRuns).mockResolvedValueOnce(page(["first"], 1, "older"))
    .mockRejectedValueOnce(new Error("Continuation unavailable")).mockReturnValueOnce(retry.promise)
    .mockResolvedValueOnce(page(["first-new"], 2, "older")).mockReturnValueOnce(refreshed.promise);
  const hook = renderHook(({ rev }) => useTestRunPages("", {}, rev), { initialProps: { rev: initial } });
  await waitFor(() => expect(hook.result.current.loaded).toBe(true));
  act(() => hook.result.current.more());
  await waitFor(() => expect(hook.result.current.error).toBe("Continuation unavailable"));
  expect(read).toHaveBeenCalledTimes(2);
  act(() => hook.result.current.retry());
  const oldSignal = read.mock.calls[2][1];
  hook.rerender({ rev: revision(2) });
  await waitFor(() => expect(read).toHaveBeenCalledTimes(5));
  expect(oldSignal?.aborted).toBe(true);
  expect(read.mock.calls.map(call => call[2])).toEqual([undefined, "older", "older", undefined, "older"]);
  await act(async () => retry.resolve(page(["stale"])));
  await act(async () => refreshed.resolve(page(["requested-older"], 2)));
  expect(hook.result.current.runs.map(run => run.runId)).toEqual(["first-new", "requested-older"]);
  expect(hook.result.current.error).toBeNull();
});

it("reaches more than 200 matches only by explicit paging and restarts the cursor for new filters", async () => {
  const ids = Array.from({ length: 257 }, (_, i) => `run-${i}`);
  const read = vi.mocked(api.readTestRuns).mockResolvedValueOnce(page(ids.slice(0, 100), 1, "second"))
    .mockResolvedValueOnce(page(ids.slice(100, 200), 1, "third"))
    .mockResolvedValueOnce(page(ids.slice(200), 1));
  const hook = renderHook(({ sessionId }) => useTestRunPages("project", { sessionId, view: "attention" }, initial),
    { initialProps: { sessionId: "owner" } });
  await waitFor(() => expect(hook.result.current.runs).toHaveLength(100));
  expect(read).toHaveBeenCalledTimes(1);
  act(() => hook.result.current.more());
  await waitFor(() => expect(hook.result.current.runs).toHaveLength(200));
  expect(read).toHaveBeenCalledTimes(2);
  act(() => hook.result.current.more());
  await waitFor(() => expect(hook.result.current.runs).toHaveLength(257));
  expect(read.mock.calls.map(call => call[2])).toEqual([undefined, "second", "third"]);
  read.mockResolvedValueOnce(page(["notify-match"]));
  hook.rerender({ sessionId: "notify" });
  await waitFor(() => expect(hook.result.current.runs.map(run => run.runId)).toEqual(["notify-match"]));
  expect(read.mock.calls[3][2]).toBeUndefined();
  expect(read.mock.calls[3][3]).toEqual({ sessionId: "notify", view: "attention" });
});

it("refreshes the first and visible loaded pages without draining history, and ignores an older in-flight reply", async () => {
  const read = vi.mocked(api.readTestRuns).mockResolvedValueOnce(page(["first"], 1, "second"))
    .mockResolvedValueOnce(page(["visible"], 1, "third"));
  const hook = renderHook(({ rev }) => useTestRunPages("", {}, rev), { initialProps: { rev: initial } });
  await waitFor(() => expect(hook.result.current.loaded).toBe(true));
  act(() => hook.result.current.more());
  await waitFor(() => expect(hook.result.current.runs).toHaveLength(2));
  act(() => hook.result.current.visible(["visible"]));
  const stale = deferred<api.TestRunPage>();
  read.mockReturnValueOnce(stale.promise).mockResolvedValueOnce(page(["visible-v2"], 2, "third"));
  hook.rerender({ rev: revision(2) });
  await waitFor(() => expect(read).toHaveBeenCalledTimes(4));
  await waitFor(() => expect(hook.result.current.runs.some(run => run.runId === "visible-v2")).toBe(true));
  const oldSignal = read.mock.calls[2][1];
  read.mockResolvedValueOnce(page(["latest-first"], 3, "second"))
    .mockResolvedValueOnce(page(["latest-visible"], 3, "third"));
  // The same visible page remains selected even when its row ids change.
  act(() => hook.result.current.visible(["visible-v2"]));
  hook.rerender({ rev: revision(3) });
  await waitFor(() => expect(hook.result.current.runs.map(run => run.runId)).toContain("latest-first"));
  expect(oldSignal?.aborted).toBe(true);
  await act(async () => stale.resolve(page(["stale"], 2, "second")));
  expect(hook.result.current.runs.map(run => run.runId)).not.toContain("stale");
  expect(read.mock.calls.every(call => call[2] !== "third")).toBe(true);
  hook.unmount();
});

it("accepts a canonical alias replacement and later restoration without retaining a tombstone", async () => {
  const read = vi.mocked(api.readTestRuns).mockResolvedValueOnce(page(["observation"]));
  const hook = renderHook(({ rev }) => useTestRunPages("", {}, rev), { initialProps: { rev: initial } });
  await waitFor(() => expect(hook.result.current.runs[0]?.runId).toBe("observation"));
  read.mockResolvedValueOnce({ ...page([], 2), runs: [makeTestRun({ runId: "canonical", supersededRunIds: ["observation"] })] });
  hook.rerender({ rev: revision(2) });
  await waitFor(() => expect(hook.result.current.runs.map(run => run.runId)).toEqual(["canonical"]));
  read.mockResolvedValueOnce(page(["observation"], 3));
  hook.rerender({ rev: revision(3) });
  await waitFor(() => expect(hook.result.current.runs.map(run => run.runId)).toEqual(["observation"]));
});

it("reports failed reads without automatic retry and aborts on panel closure", async () => {
  const read = vi.mocked(api.readTestRuns).mockRejectedValueOnce(new Error("History unavailable"));
  const hook = renderHook(() => useTestRunPages("", {}, initial));
  await waitFor(() => expect(hook.result.current.error).toBe("History unavailable"));
  expect(read).toHaveBeenCalledTimes(1);
  const pending = deferred<api.TestRunPage>();
  read.mockReturnValueOnce(pending.promise);
  act(() => hook.result.current.retry());
  const signal = read.mock.calls[1][1];
  hook.unmount();
  expect(signal?.aborted).toBe(true);
  await act(async () => pending.resolve(page(["late"])));
  expect(read).toHaveBeenCalledTimes(2);
});
