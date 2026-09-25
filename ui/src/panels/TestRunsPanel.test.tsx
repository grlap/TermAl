// List/detail/read races for the read-only Test Runs surface. Uses wire fixtures,
// not a launcher process; HTTP encoding is tested separately at the API seam.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "../test-runs-api";
import { TestRunsContext } from "../test-runs-context";
import { makeTestRun, makeTestRunDetail } from "../test-runs-fixtures";
import type { TestRunSummary } from "../test-runs";
import { TestRunsPanel } from "./TestRunsPanel";

vi.mock("../test-runs-api", () => ({ readTestRun: vi.fn(), readTestRunLog: vi.fn() }));
afterEach(() => { cleanup(); vi.restoreAllMocks(); });
beforeEach(() => {
  vi.mocked(api.readTestRun).mockReset();
  vi.mocked(api.readTestRunLog).mockReset();
  vi.mocked(api.readTestRun).mockImplementation(async runId => ({ run: makeTestRun({ runId }), detail: makeTestRunDetail() }));
  vi.mocked(api.readTestRunLog).mockResolvedValue({ text: "tail output", truncated: true, size: 90000 });
});
function view(runs: TestRunSummary[], initialSessionId: string | null = null) {
  return <TestRunsContext.Provider value={{ runs, open: vi.fn() }}>
    <TestRunsPanel projects={[]} sessions={[]} initialSessionId={initialSessionId} />
  </TestRunsContext.Provider>;
}
function selectRun(id = "test-one") {
  fireEvent.click(within(screen.getByLabelText("Discovered runs")).getByRole("button", { name: new RegExp(id) }));
}

describe("Test Runs panel", () => {
  it("filters by state and either session role, without execution controls", () => {
    render(view([makeTestRun(), makeTestRun({ runId: "test-other", state: "passed", ownerSessionId: "other", notifySessionId: null })], "session-coordinator"));
    expect(screen.queryByText("test-other")).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Session"), { target: { value: "" } });
    expect(screen.getByText("test-other")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("State"), { target: { value: "unknown" } });
    expect(screen.getByText("No test runs match these filters.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /cancel|rerun|start run/i })).not.toBeInTheDocument();
  });
  it("shows unknown/interrupted and unrun, diagnostics and truncated log tails", async () => {
    const run = makeTestRun({ state: "unknown" });
    const base = makeTestRunDetail().stages[0];
    vi.mocked(api.readTestRun).mockResolvedValue({ run, detail: makeTestRunDetail({ stages: [
      base,
      { ...base, name: "ui-tests", state: "unrun", log: null, startedAt: null },
      { ...base, name: "failed-check", state: "failed", exitCode: 1, diagnostics: { text: "original failure", truncated: true } },
    ] }) });
    render(view([run]));
    selectRun();
    expect(await within(await screen.findByLabelText("First failing stage")).findByText("original failure")).toBeInTheDocument();
    const rows = screen.getAllByRole("row");
    expect(rows[1]).toHaveTextContent("interrupted");
    expect(rows[2]).toHaveTextContent("unrun");
    expect(screen.getByRole("button", { name: "View ui-tests log" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "View rust-tests log" }));
    expect(await screen.findByText("tail output")).toBeInTheDocument();
    expect(screen.getByText("90000 bytes total · truncated tail")).toBeInTheDocument();
    expect(screen.getByText(/Unknown does not mean passed/)).toBeInTheDocument();
  });
  it("refetches equal-summary detail changes but refreshes a log only on request", async () => {
    const run = makeTestRun();
    const rendered = render(view([run]));
    selectRun();
    fireEvent.click(await screen.findByRole("button", { name: "View rust-tests log" }));
    await screen.findByText("tail output");
    rendered.rerender(view([{ ...run }]));
    await waitFor(() => expect(api.readTestRun).toHaveBeenCalledTimes(2));
    await screen.findByRole("button", { name: "View rust-tests log" });
    expect(api.readTestRunLog).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "Refresh log tail" }));
    await waitFor(() => expect(api.readTestRunLog).toHaveBeenCalledTimes(2));
  });
  it.each(["success", "failure"] as const)("keeps same-run reading state mounted through refresh %s", async outcome => {
    const run = makeTestRun();
    const rendered = render(view([run]));
    selectRun();
    const logButton = await screen.findByRole("button", { name: "View rust-tests log" });
    fireEvent.click(logButton);
    await screen.findByText("tail output");
    const summary = screen.getByText("Commands and preflight");
    const commands = summary.closest("details")!;
    commands.open = true;
    logButton.focus();
    let resolve!: (value: Awaited<ReturnType<typeof api.readTestRun>>) => void;
    let reject!: (error: Error) => void;
    vi.mocked(api.readTestRun).mockImplementationOnce(() => new Promise((done, fail) => { resolve = done; reject = fail; }));

    rendered.rerender(view([{ ...run }]));
    expect(screen.getByRole("status")).toHaveTextContent("Refreshing details");
    expect(screen.getByText("Commands and preflight")).toBe(summary);
    expect(commands.open).toBe(true);
    expect(logButton).toHaveFocus();
    expect(logButton).toBeInTheDocument();
    expect(screen.getByText("tail output")).toBeInTheDocument();

    await act(async () => {
      if (outcome === "success") resolve({ run, detail: makeTestRunDetail({ limitations: "Updated evidence" }) });
      else reject(new Error("Detail temporarily unavailable"));
    });
    expect(screen.getByText("Commands and preflight")).toBe(summary);
    expect(commands.open).toBe(true);
    expect(logButton).toHaveFocus();
    expect(api.readTestRunLog).toHaveBeenCalledTimes(1);
    if (outcome === "success") expect(screen.getByText("Updated evidence")).toBeInTheDocument();
    else expect(screen.getByRole("alert")).toHaveTextContent("previous detail snapshot; it may be stale");
  });
  it("aborts an old run request and ignores its late result after selection changes", async () => {
    let resolve!: (value: Awaited<ReturnType<typeof api.readTestRun>>) => void;
    let oldSignal: AbortSignal | undefined;
    vi.mocked(api.readTestRun).mockImplementationOnce((_id, signal) => {
      oldSignal = signal;
      return new Promise(done => { resolve = done; });
    });
    render(view([makeTestRun(), makeTestRun({ runId: "test-two" })]));
    selectRun();
    selectRun("test-two");
    await screen.findByRole("button", { name: "View rust-tests log" });
    expect(oldSignal?.aborted).toBe(true);
    await act(async () => resolve({ run: makeTestRun(), detail: makeTestRunDetail({ limitations: "stale result" }) }));
    expect(screen.queryByText("stale result")).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "test-two" })).toBeInTheDocument();
  });
  it("aborts a removed run and presents GET failures instead of retrying automatically", async () => {
    const run = makeTestRun();
    vi.mocked(api.readTestRunLog).mockRejectedValue(new Error("Stage log not found (404)"));
    const rendered = render(view([run]));
    selectRun();
    fireEvent.click(await screen.findByRole("button", { name: "View rust-tests log" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Stage log not found (404)");
    expect(api.readTestRunLog).toHaveBeenCalledTimes(1);
    rendered.rerender(view([]));
    expect(screen.queryByLabelText("rust-tests log tail")).not.toBeInTheDocument();
  });
});
