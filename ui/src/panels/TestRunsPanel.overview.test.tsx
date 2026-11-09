import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as api from "../test-runs-api";
import { TestRunsProvider } from "../test-runs-context";
import { makeTestRun, makeTestRunDetail, makeTestRunOverview } from "../test-runs-fixtures";
import { TestRunsPanel } from "./TestRunsPanel";

vi.mock("../test-runs-api", () => ({ readTestRuns: vi.fn(), readTestRun: vi.fn(), readTestRunLog: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });
beforeEach(() => {
  vi.mocked(api.readTestRun).mockImplementation(async runId => ({ run: makeTestRun({ runId }), detail: makeTestRunDetail() }));
});
const revision = (version: number) => ({ serverInstanceId: "server", revision: version, version });
const page = (version: number, runs = [makeTestRun()]): api.TestRunPage => ({ runs, limit: 100, nextCursor: null,
  revision: version, serverInstanceId: "server" });
function view(version: number, initialRun?: { runId: string; runDir: string }, initialSessionId: string | null = null) {
  return <TestRunsProvider runs={[]} snapshotReady overview={makeTestRunOverview({ version, total: 257, running: 257, needsAttention: 257 })}
    viewRevision={revision(version)} open={vi.fn()}>
    <TestRunsPanel projects={[
      { id: "project-a", name: "Project A", rootPath: "C:/a" },
    ]} sessions={[{ id: "session-owner", name: "Owner", emoji: "", agent: "Codex", workdir: "C:/fixture",
      model: "fixture", status: "idle", preview: "", messages: [] }]} initialRun={initialRun} initialSessionId={initialSessionId} />
  </TestRunsProvider>;
}

it("waits for its first page even when the aggregate snapshot is ready", async () => {
  let resolve!: (value: api.TestRunPage) => void;
  vi.mocked(api.readTestRuns).mockImplementationOnce(() => new Promise(done => { resolve = done; }));
  render(view(1));
  expect(screen.getByRole("status")).toHaveTextContent("Loading test-run history…");
  expect(screen.getByText("Waiting for the test-run index…")).toBeInTheDocument();
  expect(screen.queryByText("No test runs match these filters.")).not.toBeInTheDocument();
  await act(async () => resolve(page(1, [])));
  expect(screen.getByText("No test runs match these filters.")).toBeInTheDocument();
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
});

it.each([false, true])("keeps a settled page during revision refresh without a loading line (empty: %s)", async empty => {
  let resolve!: (value: api.TestRunPage) => void;
  const runs = empty ? [] : [makeTestRun()];
  const read = vi.mocked(api.readTestRuns).mockResolvedValueOnce(page(1, runs))
    .mockImplementationOnce(() => new Promise(done => { resolve = done; }));
  const rendered = render(view(1));
  const content = empty ? await screen.findByText("No test runs match these filters.")
    : await screen.findByRole("button", { name: /test-one/ });
  rendered.rerender(view(2));
  await waitFor(() => expect(read).toHaveBeenCalledTimes(2));
  expect(content).toBeInTheDocument();
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  expect(screen.queryByText("Waiting for the test-run index…")).not.toBeInTheDocument();
  await act(async () => resolve(page(2, runs)));
  expect(content).toBeInTheDocument();
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  expect(read).toHaveBeenCalledTimes(2);
});

it("keeps the initial error and retry loading state readable before an empty first page", async () => {
  let resolve!: (value: api.TestRunPage) => void;
  const read = vi.mocked(api.readTestRuns).mockRejectedValueOnce(new Error("History unavailable"))
    .mockImplementationOnce(() => new Promise(done => { resolve = done; }));
  render(view(1));
  expect(await screen.findByRole("alert")).toHaveTextContent("History unavailable");
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Retry history" }));
  expect(screen.getByRole("status")).toHaveTextContent("Loading test-run history…");
  await act(async () => resolve(page(1, [])));
  expect(screen.getByText("No test runs match these filters.")).toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  expect(read).toHaveBeenCalledTimes(2);
});

it("keeps loaded rows visible with a refresh error and during its explicit retry", async () => {
  let resolve!: (value: api.TestRunPage) => void;
  const read = vi.mocked(api.readTestRuns).mockResolvedValueOnce(page(1))
    .mockRejectedValueOnce(new Error("Refresh unavailable"))
    .mockImplementationOnce(() => new Promise(done => { resolve = done; }));
  const rendered = render(view(1));
  const row = await screen.findByRole("button", { name: /test-one/ });
  rendered.rerender(view(2));
  expect(await screen.findByRole("alert")).toHaveTextContent("Refresh unavailable");
  expect(row).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Retry history" }));
  expect(row).toBeInTheDocument();
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  await act(async () => resolve(page(2)));
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(row).toBeInTheDocument();
  expect(read).toHaveBeenCalledTimes(3);
});

it("refreshes selected detail on invalidation, rejects its older reply, and keeps the exact selection after leaving the active list", async () => {
  const read = vi.mocked(api.readTestRuns).mockResolvedValueOnce(page(1));
  let oldReply!: (result: Awaited<ReturnType<typeof api.readTestRun>>) => void;
  const exact = vi.mocked(api.readTestRun).mockImplementationOnce(() => new Promise(done => { oldReply = done; }));
  const rendered = render(view(1));
  const list = screen.getByLabelText("Discovered runs");
  fireEvent.click(await within(list).findByRole("button", { name: /test-one/ }));
  await waitFor(() => expect(exact).toHaveBeenCalledTimes(1));
  const oldSignal = exact.mock.calls[0][1];
  read.mockResolvedValueOnce(page(2, []));
  exact.mockResolvedValueOnce({ run: makeTestRun({ state: "passed" }),
    detail: makeTestRunDetail({ limitations: "Current retained proof" }) });
  rendered.rerender(view(2));
  expect(await screen.findByText("Current retained proof")).toBeInTheDocument();
  expect(oldSignal?.aborted).toBe(true);
  expect(exact).toHaveBeenCalledTimes(2);
  await act(async () => oldReply({ run: makeTestRun(), detail: makeTestRunDetail({ limitations: "Stale proof" }) }));
  expect(screen.queryByText("Stale proof")).not.toBeInTheDocument();
  expect(screen.getByLabelText("Run details test-one")).toHaveTextContent("State: passed");
  expect(read).toHaveBeenCalledTimes(2);
});

it("keeps View, Project and State filters without a Session control or history scan", async () => {
  const read = vi.mocked(api.readTestRuns).mockResolvedValue(page(1));
  render(view(1));
  await screen.findByRole("button", { name: /test-one/ });
  expect(screen.getAllByRole("combobox")).toHaveLength(3);
  expect(screen.queryByRole("combobox", { name: "Session" })).not.toBeInTheDocument();
  fireEvent.change(screen.getByLabelText("View"), { target: { value: "attention" } });
  await waitFor(() => expect(read).toHaveBeenCalledTimes(2));
  fireEvent.change(screen.getByLabelText("State"), { target: { value: "unknown" } });
  await waitFor(() => expect(read).toHaveBeenCalledTimes(3));
  expect(read.mock.calls[2][2]).toBeUndefined();
  expect(read.mock.calls[2][3]).toEqual({ sessionId: "", state: "unknown", view: "attention" });
  fireEvent.change(screen.getByLabelText("Project"), { target: { value: "project-a" } });
  await waitFor(() => expect(read).toHaveBeenCalledTimes(4));
  expect(read.mock.calls[3][0]).toBe("project-a");
  expect(read.mock.calls[3][2]).toBeUndefined();
  expect(read.mock.calls[3][3]).toEqual({ sessionId: "", state: "unknown", view: "attention" });
});

it("names the session-link scope, sends it to the paged query and clears it without clearing State", async () => {
  const read = vi.mocked(api.readTestRuns)
    .mockResolvedValueOnce(page(1))
    .mockResolvedValueOnce(page(1, []))
    .mockResolvedValueOnce(page(1, [makeTestRun({ runId: "other-failed", ownerSessionId: "other", notifySessionId: null, state: "failed" })]));
  render(view(1, undefined, "session-owner"));
  await screen.findByText("test-one");
  expect(screen.getByText(/Showing runs for/)).toHaveTextContent("Owner");
  expect(read.mock.calls[0][3]).toEqual({ sessionId: "session-owner", state: "", view: "history" });
  expect(screen.queryByRole("combobox", { name: "Session" })).not.toBeInTheDocument();
  fireEvent.change(screen.getByLabelText("State"), { target: { value: "failed" } });
  await screen.findByText("No test runs match these filters.");
  expect(read.mock.calls[1][3]).toEqual({ sessionId: "session-owner", state: "failed", view: "history" });
  fireEvent.click(screen.getByRole("button", { name: "Show all runs" }));
  await screen.findByText("other-failed");
  expect(read).toHaveBeenCalledTimes(3);
  expect(read.mock.calls[2][3]).toEqual({ sessionId: "", state: "failed", view: "history" });
  expect(screen.getByLabelText("State")).toHaveValue("failed");
  expect(screen.queryByText(/Showing runs for/)).not.toBeInTheDocument();
});

it("preserves the recorded literal path when exact detail is unavailable without treating omitted rows as missing", async () => {
  vi.mocked(api.readTestRuns).mockResolvedValue(page(1, []));
  vi.mocked(api.readTestRun).mockRejectedValue(new Error("Retained detail unavailable"));
  const path = "C:/repo/$cash/$(command)/run";
  render(view(1, { runId: "retained", runDir: path }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Retained detail unavailable");
  expect(screen.getByText(path)).toBeInTheDocument();
  expect(screen.getByText("node scripts/test-launcher.mjs summary RUN_DIRECTORY")).toBeInTheDocument();
  expect(screen.queryByText(/not currently indexed/)).not.toBeInTheDocument();
  expect(api.readTestRun).toHaveBeenCalledTimes(1);
});
