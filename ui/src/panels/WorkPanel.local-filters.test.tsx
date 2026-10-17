// Real Work panel witnesses for retained-snapshot display changes. API doubles
// control page settlement; no tracker process, store or browser layout is used.
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { ApiRequestError } from "../api-request";
import { readProjectWork, readWorkDetail, readWorkMemories, type WorkItem, type WorkListResponse } from "../work-visualizer-api";
import { WorkPanel } from "./WorkPanel";

vi.mock("../work-visualizer-api", () => ({ readProjectWork: vi.fn(), readWorkDetail: vi.fn(), readWorkMemories: vi.fn() }));
const projects = [
  { id: "one", name: "One", rootPath: "/one", remoteId: "local" },
  { id: "two", name: "Two", rootPath: "/two", remoteId: "local" },
] as Project[];
const item = (id: string, extra: Partial<WorkItem> = {}): WorkItem => ({
  id, shortRef: id, title: `Title ${id}`, kind: "task", lifecycle: "open", availability: "ready",
  priority: 2, labels: ["topic"], assignedTo: null, parentId: null, updatedAt: "today",
  blockedBy: [], prerequisites: [], source: "engram", ...extra,
});
function page(items = [item("alpha"), item("beta", { kind: "bug", availability: "blocked", labels: ["other"] })],
  extra: Partial<NonNullable<WorkListResponse["page"]>> = {}): WorkListResponse {
  return { readerId: "reader", observedAt: "now", sources: [{ source: "engram", state: "ready", message: "Ready" }],
    page: { items, total: items.length, shownBefore: 0, more: false, after: null, hint: null, ...extra } };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
const mount = () => render(<WorkPanel projects={projects} focusedProjectId="one" />);
const flush = () => act(async () => {});
const change = (name: string, value: string) => fireEvent.change(screen.getByLabelText(name), { target: { value } });
const apply = () => fireEvent.click(screen.getByRole("button", { name: "Apply filters" }));
const row = (id: string) => screen.queryByRole("button", { name: `${id} — Title ${id}` });

describe("Work retained snapshot filters", () => {
  beforeEach(() => {
    vi.resetAllMocks(); localStorage.clear();
    vi.mocked(readProjectWork).mockResolvedValue(page());
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
    vi.mocked(readWorkMemories).mockResolvedValue({ source: "engram", state: "ready", message: "Ready", items: [],
      nextAfter: null, omitted: 0, exhausted: true, readerId: "reader", observedAt: "now" });
  });
  afterEach(() => vi.restoreAllMocks());

  it("applies every display control to retained rows without another list read", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page([item("alpha"), item("beta", { kind: "bug", availability: "blocked", labels: ["other"] }),
      item("ended", { lifecycle: "completed" })]));
    mount(); await screen.findByRole("button", { name: "alpha — Title alpha" });
    change("Lifecycle (loaded rows)", "closed"); expect(row("ended")).toBeInTheDocument(); expect(row("alpha")).toBeNull();
    change("Lifecycle (loaded rows)", "open");
    change("Kind (loaded rows)", "bug"); expect(row("beta")).toBeInTheDocument(); expect(row("alpha")).toBeNull();
    change("Kind (loaded rows)", "");
    fireEvent.click(screen.getByRole("button", { name: "Filter by label topic" }));
    expect(row("alpha")).toBeInTheDocument(); expect(row("beta")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Clear labels" }));
    change("Label (loaded rows)", "other"); apply(); expect(row("beta")).toBeInTheDocument(); expect(row("alpha")).toBeNull();
    change("Label (loaded rows)", ""); change("Availability", "ready"); apply();
    expect(row("alpha")).toBeInTheDocument(); expect(row("beta")).toBeNull();
    change("Availability", ""); change("Search", "BETA"); apply();
    expect(row("beta")).toBeInTheDocument(); expect(row("alpha")).toBeNull();
    change("Search", ""); apply(); change("Sort (loaded rows)", "title");
    fireEvent.click(screen.getByRole("button", { name: "Ascending" }));
    fireEvent.click(screen.getByRole("button", { name: "Table" }));
    fireEvent.click(screen.getByRole("button", { name: "Priority" }));
    for (const name of ["Hierarchy", "Labels", "Dependencies", "Memories", "Table"]) {
      fireEvent.click(screen.getByRole("button", { name })); await flush();
      expect(readProjectWork).toHaveBeenCalledTimes(1);
    }
    expect(row("alpha")).toBeInTheDocument(); expect(row("beta")).toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledWith("one", { search: "", label: "", availability: "" }, expect.any(AbortSignal), undefined);
  });

  it.each(["uuid-token", "short-token", "title-token", "label-token", "owner-token"])("searches retained %s case-insensitively", async query => {
    const matching = item("uuid-token", { shortRef: "short-token", title: "Title-token", labels: ["Label-token"], assignedTo: "Owner-token" });
    vi.mocked(readProjectWork).mockResolvedValue(page([matching, item("other")]));
    mount(); await screen.findByRole("button", { name: "short-token — Title-token" });
    change("Search", query.toUpperCase()); apply();
    expect(screen.getByRole("button", { name: "short-token — Title-token" })).toBeInTheDocument();
    expect(row("other")).toBeNull();
    expect(readProjectWork).toHaveBeenCalledTimes(1);
    expect(readWorkDetail).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Search")).toHaveAccessibleDescription(/does not search notes, gates or outcomes/);
  });

  it("matches labels exactly and availability only on open rows", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page([item("alpha", { labels: ["A,b"] }), item("beta", { labels: ["a,b"] }),
      item("ended", { lifecycle: "completed", availability: "ready", labels: ["A,b"] })]));
    mount(); await screen.findByRole("button", { name: "alpha — Title alpha" });
    change("Lifecycle (loaded rows)", ""); change("Label (loaded rows)", "A,b"); apply();
    expect(row("alpha")).toBeInTheDocument(); expect(row("ended")).toBeInTheDocument(); expect(row("beta")).toBeNull();
    change("Availability", "ready"); apply();
    expect(row("alpha")).toBeInTheDocument(); expect(row("ended")).toBeNull();
    expect(readProjectWork).toHaveBeenCalledTimes(1);
  });

  it("keeps the visible selection, control focus and scroll across filtering, sorting and views", async () => {
    mount(); const initial = await screen.findByRole("button", { name: "alpha — Title alpha" });
    fireEvent.click(initial); await screen.findByRole("complementary");
    const viewport = screen.getByRole("region", { name: "Work list" });
    viewport.scrollTop = 123; fireEvent.scroll(viewport);
    change("Search", "alpha"); apply();
    expect(row("alpha")).toBe(initial); expect(row("beta")).toBeNull();
    expect(screen.getByRole("complementary")).toBeInTheDocument(); expect(viewport.scrollTop).toBe(123);
    change("Sort (loaded rows)", "title");
    for (const name of ["Hierarchy", "Labels", "Table", "Memories", "Dependencies"]) {
      const control = screen.getByRole("button", { name }); control.focus(); fireEvent.click(control); await flush();
      expect(control).toHaveFocus();
      if (name !== "Memories") {
        expect(row("alpha")).toHaveAttribute("aria-current", "true");
        expect(screen.getByRole("complementary")).toBeInTheDocument();
        expect(screen.getByRole("region", { name: "Work list" })).toBe(viewport);
        expect(viewport.scrollTop).toBe(123);
      }
    }
    expect(readProjectWork).toHaveBeenCalledTimes(1);
    change("Search", "beta"); apply();
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
  });

  it("filters loaded rows while a continuation is pending without aborting or duplicating it", async () => {
    const next = deferred<WorkListResponse>();
    vi.mocked(readProjectWork).mockResolvedValueOnce(page([item("alpha"), item("beta")], { total: 3, more: true, after: "next" }))
      .mockReturnValueOnce(next.promise);
    mount(); await screen.findByRole("button", { name: "alpha — Title alpha" });
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    const signal = vi.mocked(readProjectWork).mock.calls[1][2];
    change("Search", "alpha"); apply();
    expect(signal.aborted).toBe(false); expect(readProjectWork).toHaveBeenCalledTimes(2);
    expect(row("alpha")).toBeInTheDocument(); expect(row("beta")).toBeNull();
    await act(async () => next.resolve(page([item("alpha-later")], { total: 3, shownBefore: 2 })));
    expect(row("alpha-later")).toBeInTheDocument(); expect(row("beta")).toBeNull();
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    expect(vi.mocked(readProjectWork).mock.calls[1][3]).toEqual({ after: "next", readerId: "reader" });
  });

  it("keeps the scroll offset when sorting moves the measured anchor row", async () => {
    // Model ordered row geometry: a saved row anchor really moves on sort,
    // so this distinguishes viewport retention from keyed anchor retention.
    const original = HTMLElement.prototype.getBoundingClientRect;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      const rect = original.call(this);
      const root = this.closest<HTMLElement>(".work-panel-list");
      if (!root) return rect;
      const index = [...root.querySelectorAll("[data-work-window-key]")].indexOf(this);
      const top = this === root ? 0 : (index < 0 ? 0 : index * 100) - root.scrollTop;
      const height = this === root ? 200 : 100;
      return { ...rect, top, y: top, height, bottom: top + height, width: 500 };
    });
    vi.mocked(readProjectWork).mockResolvedValue(page(Array.from({ length: 10 }, (_, i) => item(`item-${i}`))));
    mount(); await screen.findByRole("button", { name: "item-0 — Title item-0" });
    const viewport = screen.getByRole("region", { name: "Work list" });
    viewport.scrollTop = 300; fireEvent.scroll(viewport);
    change("Sort (loaded rows)", "title"); fireEvent.click(screen.getByRole("button", { name: "Ascending" }));
    expect(viewport.scrollTop).toBe(300);
    expect([...viewport.querySelectorAll(".work-row-ref")].map(node => node.textContent))
      .toEqual(Array.from({ length: 10 }, (_, i) => `item-${9 - i}`));
    expect(readProjectWork).toHaveBeenCalledTimes(1);
  });

  it("starts exactly one generation per Refresh, project or source change, ignoring unrelated project changes", async () => {
    const view = mount(); await screen.findByRole("button", { name: "alpha — Title alpha" });
    fireEvent.click(screen.getByRole("button", { name: "Refresh work" })); await flush();
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    change("Work project", "two"); await flush(); expect(readProjectWork).toHaveBeenCalledTimes(3);
    view.rerender(<WorkPanel projects={projects.map(project => ({ ...project, name: "Renamed" }))} focusedProjectId="one" />);
    await flush(); expect(readProjectWork).toHaveBeenCalledTimes(3);
    const changed = projects.map(project => project.id === "two" ? { ...project, rootPath: "/new-source" } : project);
    view.rerender(<WorkPanel projects={changed} focusedProjectId="one" />); await flush();
    expect(readProjectWork).toHaveBeenCalledTimes(4);
    const configured = changed.map(project => project.id === "two" ? { ...project, engram: { enabled: true, binaryPath: "/trusted/engram", home: "/new-store" } } : project);
    view.rerender(<WorkPanel projects={configured} focusedProjectId="one" />); await flush();
    expect(readProjectWork).toHaveBeenCalledTimes(5);
    expect(vi.mocked(readProjectWork).mock.calls.map(call => call[0])).toEqual(["one", "one", "two", "two", "two"]);
    expect(vi.mocked(readProjectWork).mock.calls.every(call => call[3] === undefined)).toBe(true);
  });

  it.each(["initial 409", "continuation 409", "generation mismatch"])("recovers %s only through one explicit Refresh", async failure => {
    const invalid = new ApiRequestError("request-failed", "work_catalog_cursor_invalid", { status: 409 });
    if (failure === "initial 409") vi.mocked(readProjectWork).mockRejectedValueOnce(invalid);
    else {
      vi.mocked(readProjectWork).mockResolvedValueOnce(page([item("old")], { total: 2, more: true, after: "next" }));
      if (failure === "continuation 409") vi.mocked(readProjectWork).mockRejectedValueOnce(invalid);
      else vi.mocked(readProjectWork).mockResolvedValueOnce({ ...page([item("next")], { total: 2, shownBefore: 1 }), readerId: "changed-reader" });
    }
    vi.mocked(readProjectWork).mockResolvedValueOnce(page([item("fresh")]));
    mount(); await screen.findByRole("alert");
    const failedCount = failure === "initial 409" ? 1 : 2;
    change("Search", "fresh"); apply(); await flush();
    expect(readProjectWork).toHaveBeenCalledTimes(failedCount); expect(row("old")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Refresh work" }));
    await screen.findByRole("button", { name: "fresh — Title fresh" });
    expect(readProjectWork).toHaveBeenCalledTimes(failedCount + 1);
    expect(vi.mocked(readProjectWork).mock.lastCall?.[3]).toBeUndefined();
    expect(within(screen.getByRole("region", { name: "Work list" })).queryByText(/old/)).toBeNull();
  });
});
