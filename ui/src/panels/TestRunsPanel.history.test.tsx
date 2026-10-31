// History belongs to the existing paged route; live context stays bounded.
// These tests use wire fixtures and start no backend, launcher or live process.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as api from "../test-runs-api";
import { TestRunsContext } from "../test-runs-context";
import { makeTestRun, makeTestRunDetail } from "../test-runs-fixtures";
import type { TestRunSummary } from "../test-runs";
import type { TestRunCardTarget } from "../test-run-card";
import { TestRunsPanel } from "./TestRunsPanel";

vi.mock("../test-runs-api", () => ({ readTestRuns: vi.fn(), readTestRun: vi.fn(), readTestRunLog: vi.fn() }));
afterEach(() => { cleanup(); vi.restoreAllMocks(); });
beforeEach(() => {
  vi.mocked(api.readTestRuns).mockReset();
  vi.mocked(api.readTestRun).mockReset();
  vi.mocked(api.readTestRunLog).mockReset();
  vi.mocked(api.readTestRuns).mockResolvedValue({ runs: [], limit: 100, nextCursor: null });
  vi.mocked(api.readTestRun).mockImplementation(async runId => ({
    run: makeTestRun({ runId }), detail: makeTestRunDetail({ limitations: "Exact retained proof is readable." }),
  }));
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

function view(runs: TestRunSummary[] = [], initialProjectId: string | null = null, initialRun: TestRunCardTarget | null = null) {
  return <TestRunsContext.Provider value={{ runs, open: vi.fn(), snapshotReady: true }}>
    <TestRunsPanel projects={[
      { id: "project-a", name: "Project A", rootPath: "C:/a" },
      { id: "project-b", name: "Project B", rootPath: "C:/b" },
    ]} sessions={[]} initialProjectId={initialProjectId} initialRun={initialRun} />
  </TestRunsContext.Provider>;
}

function historical(runId: string, overrides: Partial<TestRunSummary> = {}) {
  return makeTestRun({ runId, state: "passed", endedAt: "2026-09-25T12:10:00.000Z", ...overrides });
}

it("loads omitted terminal history and fetches older pages only on request", async () => {
  vi.mocked(api.readTestRuns)
    .mockResolvedValueOnce({ runs: [historical("history-first")], limit: 100, nextCursor: "older-cursor" })
    .mockResolvedValueOnce({ runs: [historical("history-second")], limit: 100, nextCursor: null });
  render(view());
  await waitFor(() => expect(api.readTestRuns).toHaveBeenCalledTimes(1));
  expect(await screen.findByText("history-first")).toBeInTheDocument();
  expect(screen.queryByText("history-second")).not.toBeInTheDocument();
  expect(api.readTestRuns).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", { name: "Load older" }));
  expect(await screen.findByText("history-second")).toBeInTheDocument();
  expect(api.readTestRuns).toHaveBeenCalledTimes(2);
  expect(vi.mocked(api.readTestRuns).mock.calls[1][2]).toBe("older-cursor");
  expect(screen.queryByRole("button", { name: "Load older" })).not.toBeInTheDocument();
});

it("aborts the old project page and ignores a late response after changing projects", async () => {
  const old = deferred<api.TestRunPage>();
  let oldSignal: AbortSignal | undefined;
  vi.mocked(api.readTestRuns).mockImplementationOnce((_project, signal) => {
    oldSignal = signal;
    return old.promise;
  }).mockResolvedValueOnce({ runs: [historical("history-b", { projectId: "project-b" })], limit: 100, nextCursor: null });
  render(view([], "project-a"));
  await waitFor(() => expect(api.readTestRuns).toHaveBeenCalledTimes(1));
  expect(vi.mocked(api.readTestRuns).mock.calls[0][0]).toBe("project-a");
  fireEvent.change(screen.getByLabelText("Project"), { target: { value: "project-b" } });
  expect(await screen.findByText("history-b")).toBeInTheDocument();
  expect(oldSignal?.aborted).toBe(true);
  await act(async () => old.resolve({ runs: [historical("stale-history-a", { projectId: "project-a" })], limit: 100, nextCursor: "stale-cursor" }));
  expect(screen.queryByText("stale-history-a")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Load older" })).not.toBeInTheDocument();
});

it("preserves newer live completion when an older page arrives and the bounded snapshot later omits it", async () => {
  const pending = deferred<api.TestRunPage>();
  vi.mocked(api.readTestRuns).mockReturnValueOnce(pending.promise);
  const live = historical("live-completed", { detailVersion: "new-completion" });
  const rendered = render(view([live]));
  await waitFor(() => expect(api.readTestRuns).toHaveBeenCalledTimes(1));
  await act(async () => pending.resolve({
    runs: [makeTestRun({ runId: live.runId, detailVersion: "old-running" }), historical("page-only")],
    limit: 100, nextCursor: null,
  }));
  expect(await screen.findByText("page-only")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: /live-completed/ })).toHaveTextContent("passed");
  rendered.rerender(view([]));
  expect(screen.getByRole("button", { name: /live-completed/ })).toHaveTextContent("passed");
  expect(screen.getByText("page-only")).toBeInTheDocument();
  expect(api.readTestRuns).toHaveBeenCalledTimes(1);
});

it("reads an explicitly selected retained identity even when no snapshot or loaded page contains it", async () => {
  const run = historical("old-card-proof", { verificationRecordIds: ["verification-retained"] });
  render(view([], null, { runId: run.runId, runDir: run.runDir }));
  expect(await screen.findByText("Exact retained proof is readable.")).toBeInTheDocument();
  expect(api.readTestRun).toHaveBeenCalledWith(run.runId, expect.any(AbortSignal));
  expect(screen.queryByRole("heading", { name: "Run not currently indexed" })).not.toBeInTheDocument();
});

it("retains the recorded literal path and safe summary template when the exact-id read fails", async () => {
  const runDir = "C:/repo/$cash/$(command)/recorded-run";
  vi.mocked(api.readTestRun).mockRejectedValueOnce(new Error("Retained run is temporarily unavailable"));
  render(view([], null, { runId: "unavailable-recorded-run", runDir }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Retained run is temporarily unavailable");
  expect(screen.getByText(runDir).textContent).toBe(runDir);
  expect(screen.getByText(/node scripts\/test-launcher.mjs summary/).textContent)
    .toBe("node scripts/test-launcher.mjs summary RUN_DIRECTORY");
  expect(api.readTestRun).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("button", { name: /cancel|rerun|start run/i })).not.toBeInTheDocument();
});

it("filters loaded history by terminal state and either session role without hiding the page continuation", async () => {
  vi.mocked(api.readTestRuns).mockResolvedValueOnce({ runs: [
    historical("history-owner", { ownerSessionId: "archived-owner", notifySessionId: null }),
    historical("history-notify", { ownerSessionId: "other", notifySessionId: "archived-owner", state: "failed" }),
  ], limit: 100, nextCursor: "more-history" });
  render(view());
  expect(await screen.findByText("history-owner")).toBeInTheDocument();
  fireEvent.change(screen.getByLabelText("Session"), { target: { value: "archived-owner" } });
  expect(screen.getByText("history-notify")).toBeInTheDocument();
  fireEvent.change(screen.getByLabelText("State"), { target: { value: "failed" } });
  expect(screen.queryByText("history-owner")).not.toBeInTheDocument();
  expect(screen.getByText("history-notify")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Load older" })).toBeInTheDocument();
  expect(api.readTestRuns).toHaveBeenCalledTimes(1);
  expect(within(screen.getByLabelText("Discovered runs")).getAllByRole("button")).toHaveLength(1);
});

it("shows a history read failure without automatically retrying", async () => {
  vi.mocked(api.readTestRuns).mockRejectedValueOnce(new Error("History is temporarily unavailable"));
  render(view());
  expect(await screen.findByRole("alert")).toHaveTextContent("History is temporarily unavailable");
  expect(api.readTestRuns).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("button", { name: /cancel|rerun|start run/i })).not.toBeInTheDocument();
});

it("aborts a pending history read when the panel closes", async () => {
  const pending = deferred<api.TestRunPage>();
  let signal: AbortSignal | undefined;
  vi.mocked(api.readTestRuns).mockImplementationOnce((_project, suppliedSignal) => {
    signal = suppliedSignal;
    return pending.promise;
  });
  const rendered = render(view());
  await waitFor(() => expect(api.readTestRuns).toHaveBeenCalledTimes(1));
  rendered.unmount();
  expect(signal?.aborted).toBe(true);
  await act(async () => pending.resolve({ runs: [historical("too-late")], limit: 100, nextCursor: null }));
  expect(screen.queryByText("too-late")).not.toBeInTheDocument();
});

it("keeps a live completion received during a page read even if the snapshot omits it before the reply", async () => {
  const pending = deferred<api.TestRunPage>();
  vi.mocked(api.readTestRuns).mockReturnValueOnce(pending.promise);
  const id = "completed-during-read";
  const rendered = render(view([makeTestRun({ runId: id })]));
  await waitFor(() => expect(api.readTestRuns).toHaveBeenCalledTimes(1));
  rendered.rerender(view([historical(id, { detailVersion: "completed" })]));
  rendered.rerender(view([]));
  await act(async () => pending.resolve({ runs: [makeTestRun({ runId: id })], limit: 100, nextCursor: null }));
  expect(screen.getByRole("button", { name: new RegExp(id) })).toHaveTextContent("passed");
});

it("deduplicates a superseded observation but accepts its explicit live restoration", async () => {
  const alias = historical("restored-observation");
  const canonical = historical("canonical-artifact", { supersededRunIds: [alias.runId] });
  vi.mocked(api.readTestRuns)
    .mockResolvedValueOnce({ runs: [alias], limit: 100, nextCursor: "restored-page" })
    .mockResolvedValueOnce({ runs: [alias], limit: 100, nextCursor: null });
  const rendered = render(view([canonical]));
  await waitFor(() => expect(api.readTestRuns).toHaveBeenCalledTimes(1));
  expect(await screen.findByRole("button", { name: "Load older" })).toBeInTheDocument();
  expect(screen.queryByText(alias.runId)).not.toBeInTheDocument();
  rendered.rerender(view([alias]));
  fireEvent.click(screen.getByRole("button", { name: "Load older" }));
  await waitFor(() => expect(api.readTestRuns).toHaveBeenCalledTimes(2));
  await waitFor(() => expect(screen.queryByRole("button", { name: "Load older" })).not.toBeInTheDocument());
  rendered.rerender(view([]));
  expect(screen.getByText(alias.runId)).toBeInTheDocument();
});
