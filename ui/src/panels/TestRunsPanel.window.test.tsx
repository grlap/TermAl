// Real panel with deterministic viewport geometry; no mocked row-window hook
// or wall-clock assertions. Every run stays in the logical filter domain.
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as api from "../test-runs-api";
import { TestRunsContext } from "../test-runs-context";
import { makeTestRun, makeTestRunDetail } from "../test-runs-fixtures";
import type { TestRunSummary } from "../test-runs";
import type { Session } from "../types";
import { TestRunsPanel } from "./TestRunsPanel";
import { WORK_WINDOW_ROWS } from "./use-work-window";

vi.mock("../test-runs-api", () => ({ readTestRuns: vi.fn(), readTestRun: vi.fn(), readTestRunLog: vi.fn() }));
let measurements: (() => void)[];
const rect = (top: number, height: number) => ({ top, bottom: top + height, left: 0, right: 300,
  width: 300, height, x: 0, y: top, toJSON() {} });
beforeEach(() => {
  measurements = [];
  vi.mocked(api.readTestRuns).mockResolvedValue({ runs: [], limit: 100, nextCursor: null });
  vi.mocked(api.readTestRun).mockImplementation(async runId => ({ run: makeTestRun({ runId }), detail: makeTestRunDetail() }));
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    const root = this.closest<HTMLElement>('[aria-label="Discovered runs"]');
    if (!root || this === root) return rect(0, 120);
    let top = -root.scrollTop;
    for (let node: HTMLElement | null = this; node && node !== root; node = node.parentElement)
      for (let previous = node.previousElementSibling as HTMLElement | null; previous; previous = previous.previousElementSibling as HTMLElement | null)
        top += previous.dataset.workWindowKey ? 40 : Number.parseFloat(previous.style.height) || 0;
    return rect(top, this.dataset.workWindowKey ? 40 : 0);
  });
  vi.stubGlobal("ResizeObserver", class {
    private nodes = new Set<Element>();
    constructor(callback: ResizeObserverCallback) {
      measurements.push(() => callback([...this.nodes].map(target => ({ target })) as ResizeObserverEntry[], this as unknown as ResizeObserver));
    }
    observe(node: Element) { this.nodes.add(node); }
    unobserve(node: Element) { this.nodes.delete(node); }
    disconnect() { this.nodes.clear(); }
  });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.clearAllMocks(); });
const measure = () => act(() => measurements.forEach(callback => callback()));
const id = (index: number) => `run-${String(index).padStart(3, "0")}`;
const runs = () => Array.from({ length: 250 }, (_, index) => makeTestRun({ runId: id(index),
  ownerSessionId: `owner-${index}`, notifySessionId: null, state: index === 249 ? "failed" : "passed",
  endedAt: "2026-09-25T12:10:00.000Z" }));
const session = (name: string): Session => ({ id: "named-owner", name, emoji: "", agent: "Codex",
  workdir: "C:/fixture", model: "fixture", status: "idle", preview: "", messages: [] });
function view(rows: TestRunSummary[], options: { selected?: number; sessions?: Session[]; sessionId?: string } = {}) {
  return <TestRunsContext.Provider value={{ runs: rows, open: vi.fn(), snapshotReady: true }}>
    <TestRunsPanel projects={[]} sessions={options.sessions ?? []}
      initialSessionId={options.sessionId}
      initialRun={options.selected === undefined ? null : { runId: id(options.selected), runDir: "C:/fixture/run" }} />
  </TestRunsContext.Provider>;
}
const list = () => screen.getByLabelText("Discovered runs");
const buttons = () => within(list()).getAllByRole("button");
const row = (index: number) => within(list()).getByRole("button", { name: new RegExp(id(index)) });

it("bounds mounted history while state filtering still reaches a run beyond the window", async () => {
  render(view(runs()));
  await waitFor(() => expect(api.readTestRuns).toHaveBeenCalledTimes(1));
  measure();
  expect(buttons().length).toBeLessThanOrEqual(WORK_WINDOW_ROWS + 3);
  expect(within(list()).queryByText(id(249))).not.toBeInTheDocument();
  fireEvent.change(screen.getByLabelText("State"), { target: { value: "failed" } });
  expect(row(249)).toBeInTheDocument();
  expect(row(249)).toHaveTextContent("Owner: owner-249");
  expect(buttons()).toHaveLength(1);
  fireEvent.click(row(249));
  expect(await screen.findByRole("heading", { name: id(249) })).toBeInTheDocument();
});

it("moves Tab and Shift+Tab focus across unmounted row boundaries without top-aligning visible neighbors", async () => {
  render(view(runs()));
  await waitFor(() => expect(api.readTestRuns).toHaveBeenCalledTimes(1));
  measure();
  const mounted = buttons();
  const last = mounted[mounted.length - 1];
  const index = Number(last.textContent!.match(/run-(\d{3})/)![1]);
  expect(index).toBeLessThan(249);
  expect(within(list()).queryByText(id(index + 1))).not.toBeInTheDocument();
  act(() => last.focus());
  fireEvent.keyDown(last, { key: "Tab" });
  expect(row(index + 1)).toHaveFocus();
  measure();
  fireEvent.keyDown(row(index + 1), { key: "Tab", shiftKey: true });
  expect(row(index)).toHaveFocus();
  const currentTop = list().scrollTop;
  fireEvent.keyDown(row(index), { key: "Tab" });
  expect(row(index + 1)).toHaveFocus();
  expect(list().scrollTop).toBe(currentTop);
});

it("retains selected details when scrolling, then resets a shrinking filter without stealing its focus", async () => {
  render(view(runs(), { selected: 200 }));
  const heading = await screen.findByRole("heading", { name: id(200) });
  measure();
  list().scrollTop = 12000;
  fireEvent.scroll(list());
  measure();
  expect(screen.getByRole("heading", { name: id(200) })).toBe(heading);
  expect(buttons().length).toBeLessThanOrEqual(WORK_WINDOW_ROWS + 3);
  const filter = screen.getByLabelText("State");
  act(() => filter.focus());
  fireEvent.change(filter, { target: { value: "failed" } });
  expect(list().scrollTop).toBe(0);
  expect(row(249)).toBeInTheDocument();
  expect(filter).toHaveFocus();
});

it("updates session labels after source changes while retaining the session-link scope", async () => {
  const original = runs();
  original[249] = { ...original[249], ownerSessionId: "named-owner" };
  const rendered = render(view(original, { sessions: [session("Before rename")], sessionId: "named-owner" }));
  await waitFor(() => expect(api.readTestRuns).toHaveBeenCalledTimes(1));
  expect(row(249)).toHaveTextContent("Owner: Before rename");
  rendered.rerender(view([...original, makeTestRun({ runId: "added", ownerSessionId: "new-owner", notifySessionId: null })],
    { sessions: [session("After rename")], sessionId: "named-owner" }));
  expect(row(249)).toHaveTextContent("Owner: After rename");
  expect(screen.queryByText("Owner: Before rename")).not.toBeInTheDocument();
  expect(screen.queryByText("added")).not.toBeInTheDocument();
  expect(buttons()).toHaveLength(1);
  expect(screen.queryByRole("combobox", { name: "Session" })).not.toBeInTheDocument();
});

it("keeps the visible row anchored when a new live head and an older history page arrive", async () => {
  const original = runs();
  const rendered = render(view(original));
  await waitFor(() => expect(api.readTestRuns).toHaveBeenCalledTimes(1));
  measure();
  list().scrollTop = 800;
  fireEvent.scroll(list());
  measure();
  const anchor = buttons().find(button => button.parentElement!.getBoundingClientRect().top >= 0)!;
  const anchorTop = anchor.parentElement!.getBoundingClientRect().top;
  const next = [makeTestRun({ runId: "new-live-head", startedAt: "2026-11-01T12:00:00.000Z" }), ...original,
    makeTestRun({ runId: "older-page", startedAt: "2025-01-01T12:00:00.000Z" })];
  rendered.rerender(view(next));
  measure();
  expect(anchor).toBeInTheDocument();
  expect(anchor.parentElement!.getBoundingClientRect().top).toBe(anchorTop);
  expect(buttons().length).toBeLessThanOrEqual(WORK_WINDOW_ROWS + 3);
});
