// Real-panel paging/window integration with controlled viewport demand. No
// tracker process or live store is involved.
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { ApiRequestError } from "../api-request";
import { readProjectWork, readWorkDetail, type WorkItem, type WorkListResponse } from "../work-visualizer-api";
import { WorkPanel } from "./WorkPanel";
import { WORK_WINDOW_ROWS } from "./use-work-window";

vi.mock("../work-visualizer-api", () => ({ readProjectWork: vi.fn(), readWorkDetail: vi.fn() }));
const projects = [{ id: "project", name: "Project", rootPath: "/project", remoteId: "local" }] as Project[];
function page(start = 0, total = 40, prefix = "item-"): WorkListResponse {
  const end = Math.min(start + 20, total);
  const items = Array.from({ length: end - start }, (_, i): WorkItem => ({
    id: `${prefix}${start + i}`, shortRef: `${prefix}${start + i}`, title: `Task ${start + i}`, kind: "task",
    lifecycle: "open", availability: "ready", priority: 2, labels: ["topic"], assignedTo: null,
    parentId: null, updatedAt: "today", blockedBy: [], source: "engram", prerequisites: [],
  }));
  return { readerId: "reader", observedAt: "now", sources: [{ source: "engram", state: "ready", message: "Open work" }],
    page: { items, total, shownBefore: start, more: end < total, after: end < total ? `c${end}` : null, hint: null } };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
const mount = () => render(<WorkPanel projects={projects} focusedProjectId="project" />);
const loaded = (count: number, total = 40) => screen.findByText(new RegExp(`${count} of ${total} Engram items loaded`));

describe("Work serial paging", () => {
  beforeEach(() => {
    vi.resetAllMocks(); localStorage.clear();
    // jsdom has no layout. Give the content its real scroll-relative origin;
    // row heights remain the production estimate until a measurement arrives.
    const original = HTMLElement.prototype.getBoundingClientRect;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      const rect = original.call(this);
      if (!this.matches(".work-tree, .work-table-scroll, .work-label-window")) return rect;
      const top = -(this.closest(".work-panel-list")?.scrollTop ?? 0);
      return { ...rect, top, bottom: top + rect.height, y: top };
    });
  });
  afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });

  it("loads serially without scrolling and preserves order, existing rows, scroll and selection", async () => {
    const first = deferred<WorkListResponse>(), second = deferred<WorkListResponse>();
    vi.mocked(readProjectWork).mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const { container } = mount();
    expect(screen.getByRole("status")).toHaveTextContent("Reading work");
    await act(async () => first.resolve(page()));
    await loaded(20);
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
    const existing = screen.getByRole("button", { name: "item-0 — Task 0" });
    fireEvent.click(existing);
    await screen.findByRole("complementary", { name: "Work item details" });
    const viewport = screen.getByRole("region", { name: "Work list" });
    viewport.scrollTop = 300;
    fireEvent.scroll(viewport);
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    expect(screen.getByText("Loading more work…")).toBeInTheDocument();
    await act(async () => second.resolve(page(20)));
    expect(viewport.scrollTop).toBe(300);
    expect(screen.getByRole("button", { name: "item-0 — Task 0" })).toBe(existing);
    expect(existing).toHaveAttribute("aria-current", "true");
    expect(screen.getByRole("complementary", { name: "Work item details" })).toBeInTheDocument();
    // Source order shows how the pages merged: the second page follows the
    // first. (The default Priority sort would order these equal P2 rows by id.)
    fireEvent.change(screen.getByRole("combobox", { name: "Sort (loaded rows)" }), { target: { value: "" } });
    expect([...container.querySelectorAll(".work-row-ref")].map(node => node.textContent))
      .toEqual(Array.from({ length: 40 }, (_, i) => `item-${i}`));
    expect(screen.getByText("End of work list.")).toBeInTheDocument();
    await act(async () => {});
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    expect(readProjectWork).toHaveBeenLastCalledWith("project", expect.any(Object), expect.any(AbortSignal), { after: "c20", readerId: "reader" });
    expect(screen.queryByRole("button", { name: "Load more work" })).not.toBeInTheDocument();
  });

  it("pauses after a page error and retries the same cursor once per activation", async () => {
    const retry = deferred<WorkListResponse>();
    vi.mocked(readProjectWork).mockResolvedValueOnce(page()).mockRejectedValueOnce(new Error("Offline"))
      .mockReturnValueOnce(retry.promise);
    mount(); await loaded(20);
    expect(await screen.findByRole("alert")).toHaveTextContent("Offline");
    await act(async () => {});
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    const button = screen.getByRole("button", { name: "Retry" });
    act(() => { button.click(); button.click(); });
    expect(readProjectWork).toHaveBeenCalledTimes(3);
    expect(vi.mocked(readProjectWork).mock.calls[2][3]).toEqual(vi.mocked(readProjectWork).mock.calls[1][3]);
    await act(async () => retry.resolve(page(20)));
    await loaded(40);
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it.each(["Dependencies", "Hierarchy", "Labels", "Table"])("preserves row DOM and details across the window threshold in %s", async presentation => {
    const all = [0, 20, 40, 60].flatMap(start => page(start, 80).page!.items);
    const updated = "2026-10-15T18:35:12.123456789Z";
    all[0] = { ...all[0], updatedAt: updated };
    const first = page(0, 80), next = page(60, 80), continuation = deferred<WorkListResponse>();
    first.page = { ...first.page!, items: all.slice(0, 55), after: "c55" };
    next.page = { ...next.page!, items: all.slice(55), shownBefore: 55 };
    vi.mocked(readProjectWork).mockResolvedValueOnce(first).mockReturnValueOnce(continuation.promise);
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
    mount(); await loaded(55, 80);
    fireEvent.click(screen.getByRole("button", { name: presentation }));
    const existing = screen.getByRole("button", { name: "item-0 — Task 0" });
    const metadata = existing.closest("li, td")!.querySelector(".work-row-secondary .work-row-updated")!;
    expect(metadata).toHaveAttribute("title", updated);
    expect(metadata).toHaveTextContent(/Last updated \d{4}-\d{2}-\d{2} \d{2}:\d{2}/);
    expect(metadata.querySelector("time")).toHaveAttribute("dateTime", "2026-10-15T18:35:12.123Z");
    expect(metadata.querySelector("time")).toHaveAttribute("title", updated);
    fireEvent.click(existing);
    await screen.findByRole("complementary", { name: "Work item details" });
    const viewport = screen.getByRole("region", { name: "Work list" });
    viewport.scrollTop = 300; fireEvent.scroll(viewport);
    await act(async () => continuation.resolve(next));
    await loaded(80, 80);
    expect(screen.getByRole("button", { name: "item-0 — Task 0" })).toBe(existing);
    expect(existing.closest("li, td")!.querySelector(".work-row-updated")).toBe(metadata);
    expect(existing).toHaveAttribute("aria-current", "true");
    expect(screen.getByRole("complementary", { name: "Work item details" })).toBeInTheDocument();
    expect(viewport.scrollTop).toBe(300);
  });

  it("shows a changed-catalog reload state and never silently restarts", async () => {
    vi.mocked(readProjectWork).mockResolvedValueOnce(page()).mockRejectedValueOnce(
      new ApiRequestError("request-failed", "work_catalog_cursor_invalid", { status: 409 }));
    mount();
    expect(await screen.findByRole("alert")).toHaveTextContent("Work list changed");
    expect(screen.queryByRole("button", { name: "item-0 — Task 0" })).not.toBeInTheDocument();
    await act(async () => {});
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("button", { name: "Refresh work" })).toBeEnabled();
  });

  it("defaults to Open and exposes every closed lifecycle through the filter and existing Updated sort", async () => {
    const response = page(0, 4);
    response.page!.items = response.page!.items.map((item, i) => ({ ...item,
      lifecycle: ["open", "completed", "cancelled", "superseded"][i], updatedAt: `2026-10-${10 + i}T00:00:00Z` }));
    vi.mocked(readProjectWork).mockResolvedValue(response);
    const view = mount(); await loaded(4, 4);
    expect(screen.getByLabelText("Lifecycle (loaded rows)")).toHaveValue("open");
    expect(view.container.querySelectorAll(".work-row-title")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Table" }));
    fireEvent.change(screen.getByLabelText("Lifecycle (loaded rows)"), { target: { value: "closed" } });
    fireEvent.click(screen.getByRole("button", { name: "Updated" }));
    expect(screen.getByRole("columnheader", { name: "Updated" })).toHaveAttribute("aria-sort", "descending");
    expect([...view.container.querySelectorAll(".work-row-title")].map(node => node.textContent))
      .toEqual(["item-3 — Task 3", "item-2 — Task 2", "item-1 — Task 1"]);
    for (const [i, lifecycle] of ["completed", "cancelled", "superseded"].entries()) {
      fireEvent.change(screen.getByLabelText("Lifecycle (loaded rows)"), { target: { value: lifecycle } });
      expect(screen.getByRole("button", { name: `item-${i + 1} — Task ${i + 1}` })).toBeInTheDocument();
      expect(view.container.querySelectorAll(".work-row-title")).toHaveLength(1);
    }
    fireEvent.change(screen.getByLabelText("Lifecycle (loaded rows)"), { target: { value: "" } });
    expect(view.container.querySelectorAll(".work-row-title")).toHaveLength(4);
    expect(readProjectWork).toHaveBeenCalledTimes(1);
  });

  it("discloses a byte-bounded page without repeatedly asking for it", async () => {
    const bounded = page(); bounded.page!.after = null; bounded.page!.hint = "Receipt limit reached";
    vi.mocked(readProjectWork).mockResolvedValue(bounded);
    mount(); await loaded(20);
    expect(screen.getByText(/byte-limited page has no continuation/)).toBeInTheDocument();
    expect(screen.getByText("Receipt limit reached")).toBeInTheDocument();
    await act(async () => {});
    expect(readProjectWork).toHaveBeenCalledTimes(1);
  });

  it("keeps 1,200 incrementally loaded rows in a bounded window in every presentation", async () => {
    const replies = Array.from({ length: 59 }, () => deferred<WorkListResponse>());
    vi.mocked(readProjectWork).mockImplementation(async (_project, _filters, _signal, continuation) =>
      continuation ? replies[Number(continuation.after.slice(1)) / 20 - 1].promise : page(0, 1200));
    const { container } = mount(); await loaded(20, 1200);
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    for (let i = 0; i < 59; i++) {
      expect(readProjectWork).toHaveBeenCalledTimes(i + 2);
      await act(async () => replies[i].resolve(page((i + 1) * 20, 1200)));
    }
    await loaded(1200, 1200);
    expect(readProjectWork).toHaveBeenCalledTimes(60);
    expect(vi.mocked(readProjectWork).mock.calls.map(call => call[3]?.after))
      .toEqual([undefined, ...Array.from({ length: 59 }, (_, i) => `c${(i + 1) * 20}`)]);
    for (const view of ["Dependencies", "Hierarchy", "Labels", "Table"]) {
      fireEvent.click(screen.getByRole("button", { name: view }));
      expect(container.querySelectorAll(".work-row-title").length).toBeGreaterThan(0);
      expect(container.querySelectorAll(".work-row-title").length).toBeLessThanOrEqual(WORK_WINDOW_ROWS + 2);
    }
    const viewport = screen.getByRole("region", { name: "Work list" });
    viewport.scrollTop = 90_000;
    fireEvent.scroll(viewport);
    expect(container.querySelectorAll(".work-row-title").length).toBeLessThanOrEqual(WORK_WINDOW_ROWS + 2);
    expect(screen.queryByRole("button", { name: "item-0 — Task 0" })).not.toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(60);
  });

  it.each(["Search", "Label (loaded rows)", "Availability"])("keeps the pending continuation across %s changes", async name => {
    const next = deferred<WorkListResponse>();
    vi.mocked(readProjectWork).mockResolvedValueOnce(page()).mockReturnValueOnce(next.promise);
    mount(); await loaded(20);
    const signal = vi.mocked(readProjectWork).mock.calls[1][2];
    fireEvent.change(screen.getByLabelText(name), { target: { value: name === "Availability" ? "ready" : name === "Search" ? "Task 2" : "topic" } });
    fireEvent.click(screen.getByRole("button", { name: "Apply filters" }));
    expect(signal.aborted).toBe(false);
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    await act(async () => next.resolve(page(20)));
    await loaded(40);
    expect(screen.getByRole("button", { name: "item-20 — Task 20" })).toBeInTheDocument();
    if (name === "Search") expect(screen.queryByRole("button", { name: "item-0 — Task 0" })).not.toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(2);
  });
});
