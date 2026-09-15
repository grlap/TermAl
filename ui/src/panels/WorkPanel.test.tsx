import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { ApiRequestError } from "../api-request";
import { readProjectWork, readWorkBeadsDetail, readWorkDetail, type WorkItem, type WorkListResponse, type WorkDetailResponse, type WorkPage } from "../work-visualizer-api";
import { WORK_AUTO_LOAD_TARGET_ROWS } from "./use-work-list";
import { WorkPanel } from "./WorkPanel";

vi.mock("../work-visualizer-api", () => ({ readProjectWork: vi.fn(), readWorkDetail: vi.fn(), readWorkBeadsDetail: vi.fn() }));
const projects = [
  { id: "p-one", name: "One", rootPath: "/one", remoteId: "local" },
  { id: "p-two", name: "Two", rootPath: "/two", remoteId: "local" },
] as Project[];
function page(title = "Visible task", after: string | null = null): WorkListResponse {
  return { sources: [{ source: "engram", state: "ready", message: "Engram reads use the host reader over the validated project store" }], readerId: "host:reader", observedAt: "now",
    page: { items: [{ id: title, shortRef: "w-one", title, kind: "bug", lifecycle: "open", availability: "blocked", priority: 1, labels: ["decision"], assignedTo: "actor", parentId: null, updatedAt: "today", blockedBy: [], source: "engram", prerequisites: [] }], total: after ? 2 : 1, shownBefore: 0, more: !!after, after, hint: null },
    beads: null };
}
function bead(id: string, title: string, extra: Partial<WorkItem> = {}): WorkItem {
  return { id, shortRef: id, title, kind: "task", lifecycle: "open", availability: "ready", priority: 2, labels: [], assignedTo: null, parentId: null, updatedAt: "today", blockedBy: [], source: "beads", prerequisites: [], ...extra };
}
// Mirrors the Rust fixture: a goal blocked by one loaded and one absent
// prerequisite, its parent, and the ready item it waits for.
function beadsPage(): WorkPage {
  return { items: [
    bead("tm-root", "Root epic", { kind: "feature", availability: "active", assignedTo: "Termal::Codex" }),
    bead("tm-root.1", "Blocked child", { parentId: "tm-root", availability: "blocked", priority: 1, prerequisites: [{ id: "tm-free", satisfied: false }, { id: "tm-closed", satisfied: true }] }),
    bead("tm-free", "Ready bug", { kind: "bug", priority: 3 }),
  ], total: 3, shownBefore: 0, more: false, after: null, hint: null };
}
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(r => { resolve = r; }); return { promise, resolve }; }
function showTable() { fireEvent.click(screen.getByRole("button", { name: "Table" })); }

describe("WorkPanel", () => {
  beforeEach(() => { vi.resetAllMocks(); localStorage.clear(); });

  it("uses the focused project and renders independent lifecycle, availability and assignment", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    render(<WorkPanel projects={projects} focusedProjectId="p-two" />);
    expect(screen.getByRole("combobox", { name: "Work project" })).toHaveValue("p-two");
    expect(await screen.findByRole("button", { name: "w-one — Visible task" })).toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledWith("p-two", { search: "", label: "", availability: "" }, expect.any(AbortSignal), undefined);
    expect(screen.getByRole("button", { name: "Dependencies", pressed: true })).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Work view" })).toBeInTheDocument();
    showTable();
    expect(screen.getByRole("button", { name: "Table", pressed: true })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "open" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "blocked" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Assignment" })).toBeInTheDocument();
  });

  it("keeps the chosen view and sort across source-filter submissions", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-one — Visible task" });
    showTable();
    fireEvent.change(screen.getByRole("combobox", { name: "Sort (loaded rows)" }), { target: { value: "title" } });
    // Applying source filters starts a new read generation; the presentation
    // choices are not part of it.
    fireEvent.click(screen.getByRole("button", { name: "Apply filters" }));
    await waitFor(() => expect(readProjectWork).toHaveBeenCalledTimes(2));
    await screen.findByRole("button", { name: "w-one — Visible task" });
    expect(screen.getByRole("button", { name: "Table", pressed: true })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Work table scroll area" })).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Sort (loaded rows)" })).toHaveValue("title");
  });

  it("sorts the table from a clicked header, reverses on the second click and shares one state with the sort control", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({ sources: [{ source: "beads", state: "ready", message: "bd" }], readerId: null, observedAt: "now", page: null, beads: beadsPage() });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "tm-root — Root epic" });
    showTable();
    const column = (index: number) => within(screen.getByRole("table")).getAllByRole("row").slice(1).map(row => within(row).getAllByRole("cell")[index]!.textContent);
    const header = (name: string) => screen.getByRole("columnheader", { name });
    // Source order until a header is chosen; every header is a button, so it
    // is reachable from the keyboard, and reports its state through aria-sort.
    expect(column(3)).toEqual(["feature", "task", "bug"]);
    expect(header("Kind")).toHaveAttribute("aria-sort", "none");
    fireEvent.click(screen.getByRole("button", { name: "Kind" }));
    expect(column(3)).toEqual(["bug", "feature", "task"]);
    expect(header("Kind")).toHaveAttribute("aria-sort", "ascending");
    expect(screen.getByRole("combobox", { name: "Sort (loaded rows)" })).toHaveValue("kind");
    expect(screen.getByRole("button", { name: "Ascending" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Kind" }));
    expect(column(3)).toEqual(["task", "feature", "bug"]);
    expect(header("Kind")).toHaveAttribute("aria-sort", "descending");
    // The direction control reverses the same sort; another header replaces it.
    fireEvent.click(screen.getByRole("button", { name: "Descending" }));
    expect(column(3)).toEqual(["bug", "feature", "task"]);
    fireEvent.click(screen.getByRole("button", { name: "Priority" }));
    expect(column(0)).toEqual(["P1", "P2", "P3"]);
    expect(header("Priority")).toHaveAttribute("aria-sort", "ascending");
    expect(header("Kind")).toHaveAttribute("aria-sort", "none");
    fireEvent.click(screen.getByRole("button", { name: "Updated" }));
    expect(header("Updated")).toHaveAttribute("aria-sort", "descending");
    // Sorting only reorders the loaded rows: no new read, same row identities.
    expect(readProjectWork).toHaveBeenCalledTimes(1);
    expect(column(1).map(text => text?.slice(0, 7)).sort()).toEqual(["tm-free", "tm-root", "tm-root"]);
    fireEvent.change(screen.getByRole("combobox", { name: "Sort (loaded rows)" }), { target: { value: "" } });
    expect(column(3)).toEqual(["feature", "task", "bug"]);
    expect(header("Updated")).toHaveAttribute("aria-sort", "none");
  });

  it("aborts and ignores a late response after switching projects", async () => {
    const old = deferred<WorkListResponse>();
    vi.mocked(readProjectWork).mockReturnValueOnce(old.promise).mockResolvedValueOnce(page("New project"));
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    const signal = vi.mocked(readProjectWork).mock.calls[0][2];
    fireEvent.change(screen.getByRole("combobox", { name: "Work project" }), { target: { value: "p-two" } });
    await screen.findByRole("button", { name: "w-one — New project" });
    expect(signal.aborted).toBe(true);
    await act(async () => old.resolve(page("Old project")));
    expect(screen.queryByText(/Old project/)).not.toBeInTheDocument();
    expect(localStorage.getItem("termal-work-project")).toBe("p-two");
  });

  it("does not turn a failed read into an empty list", async () => {
    vi.mocked(readProjectWork).mockRejectedValue(new ApiRequestError("request-failed", "engram work ls: invalid JSON", { status: 502 }));
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    expect(await screen.findByRole("alert")).toHaveTextContent("engram work ls: invalid JSON");
    expect(screen.queryByText(/No work items match/)).not.toBeInTheDocument();
  });

  it("retries unchanged filters after an error and aborts the previous request", async () => {
    vi.mocked(readProjectWork).mockRejectedValueOnce(new ApiRequestError("request-failed", "Work reads busy", { status: 429 }))
      .mockResolvedValueOnce(page("Retried"));
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("alert");
    const firstSignal = vi.mocked(readProjectWork).mock.calls[0][2];
    fireEvent.click(screen.getByRole("button", { name: "Apply filters" }));
    await screen.findByRole("button", { name: "w-one — Retried" });
    expect(firstSignal.aborted).toBe(true);
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    showTable();
    expect(screen.getByRole("region", { name: "Work table scroll area" })).toBeInTheDocument();
  });

  it("preserves an explicit project selection across tab remounts", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    const first = render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-one — Visible task" });
    fireEvent.change(screen.getByRole("combobox", { name: "Work project" }), { target: { value: "p-two" } });
    await screen.findByRole("button", { name: "w-one — Visible task" });
    first.unmount();
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    expect(screen.getByRole("combobox", { name: "Work project" })).toHaveValue("p-two");
    await screen.findByRole("button", { name: "w-one — Visible task" });
    expect(readProjectWork).toHaveBeenLastCalledWith("p-two", expect.any(Object), expect.any(AbortSignal), undefined);
  });

  it("drops the old generation when a continuation becomes invalid", async () => {
    vi.mocked(readProjectWork).mockResolvedValueOnce(page("Old item", "opaque"))
      .mockRejectedValueOnce(new ApiRequestError("request-failed", "work_catalog_cursor_invalid", { status: 409 }));
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    // The first page carries a cursor, so its continuation is read on its own.
    expect(await screen.findByRole("alert")).toHaveTextContent("work_catalog_cursor_invalid");
    expect(screen.queryByText(/Old item/)).not.toBeInTheDocument();
    expect(readProjectWork).toHaveBeenLastCalledWith("p-one", expect.any(Object), expect.any(AbortSignal), { after: "opaque", readerId: "host:reader" });
    expect(readProjectWork).toHaveBeenCalledTimes(2);
  });

  it.each(["removed", "initially empty"])("adopts a stable fallback when selection is %s", async (scenario) => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
    const removed = { ...projects[0], id: "removed" };
    const view = render(<WorkPanel projects={scenario === "removed" ? [removed, ...projects] : []} focusedProjectId="removed" />);
    if (scenario === "removed") await screen.findByRole("button", { name: "w-one — Visible task" });
    view.rerender(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    fireEvent.click(await screen.findByRole("button", { name: "w-one — Visible task" }));
    await screen.findByRole("complementary", { name: "Work item details" });
    fireEvent.change(screen.getByRole("textbox", { name: "Search" }), { target: { value: "keep draft" } });
    const reads = vi.mocked(readProjectWork).mock.calls.length;
    view.rerender(<WorkPanel projects={projects} focusedProjectId="p-two" />);
    expect(screen.getByRole("combobox", { name: "Work project" })).toHaveValue("p-one");
    expect(screen.getByRole("textbox", { name: "Search" })).toHaveValue("keep draft");
    expect(screen.getByRole("complementary", { name: "Work item details" })).toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(reads);
    expect(localStorage.getItem("termal-work-project")).toBeNull();
  });

  it.each(["total", "shownBefore", "reader", "duplicate"])("rejects a successful list continuation with changed %s", async (field) => {
    const next = page("Second"); next.page!.total = 2; next.page!.shownBefore = 1;
    if (field === "total") next.page!.total = 3;
    if (field === "shownBefore") next.page!.shownBefore = 0;
    if (field === "reader") next.readerId = "different-reader";
    if (field === "duplicate") next.page!.items[0].id = "First";
    vi.mocked(readProjectWork).mockResolvedValueOnce(page("First", "opaque")).mockResolvedValueOnce(next);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    // The continuation is read automatically; a changed generation is still refused.
    expect(await screen.findByRole("alert")).toHaveTextContent("Work page generation changed. Refresh");
    expect(screen.queryByRole("button", { name: /w-one/ })).not.toBeInTheDocument();
    expect(screen.queryByText("First")).not.toBeInTheDocument();
    expect(screen.queryByText("Second")).not.toBeInTheDocument();
  });

  it.each(["projectPosition", "total", "newer", "duplicate"])("rejects a successful notes continuation with changed %s", async (field) => {
    const detail: WorkDetailResponse = { status: null, holder: null, heldUntil: null,
      notes: [{ locator: "first", summary: "First note", family: "notes", kind: "note", by: null, createdAt: "now", bodyOmitted: false, summaryTruncated: false }],
      notesWindow: { total: 2, shown: 1, newer: 0, older: 1, after: "opaque", readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } };
    const next: WorkDetailResponse = { ...detail, notes: [{ ...detail.notes[0], locator: "second", summary: "Second note" }],
      notesWindow: { ...detail.notesWindow, newer: 1, older: 0, after: null, readCut: { ...detail.notesWindow.readCut, observedAt: "later" } } };
    if (field === "projectPosition") next.notesWindow.readCut.projectPosition = 2;
    if (field === "total") next.notesWindow.total = 3;
    if (field === "newer") next.notesWindow.newer = 0;
    if (field === "duplicate") next.notes[0].locator = "first";
    vi.mocked(readProjectWork).mockResolvedValue(page());
    vi.mocked(readWorkDetail).mockResolvedValueOnce(detail).mockResolvedValueOnce(next);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    fireEvent.click(await screen.findByRole("button", { name: "w-one — Visible task" }));
    fireEvent.click(await screen.findByRole("button", { name: "Load older notes" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Notes window changed. Reload details");
    expect(screen.queryByText("First note")).not.toBeInTheDocument();
    expect(screen.queryByText("Second note")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Load older notes" })).not.toBeInTheDocument();
  });

  it("shows a byte-limited page without repeatedly reading it", async () => {
    // `more` without a cursor is the byte-limited case: automatic reading
    // ahead stops here with the hint, never a retry loop.
    const limited = page(); limited.page = { items: [], total: 1, shownBefore: 0, more: true, after: null, hint: "First row exceeds budget; use show" };
    vi.mocked(readProjectWork).mockResolvedValue(limited);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    expect(await screen.findByText("First row exceeds budget; use show")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Load more work" })).not.toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(1);
  });

  it("reads the continuation on its own after the first page and never more than one at a time", async () => {
    const pending = deferred<WorkListResponse>();
    vi.mocked(readProjectWork).mockResolvedValueOnce(page("First", "opaque")).mockReturnValueOnce(pending.promise);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-one — First" });
    await waitFor(() => expect(readProjectWork).toHaveBeenCalledTimes(2));
    expect(readProjectWork).toHaveBeenLastCalledWith("p-one", expect.any(Object), expect.any(AbortSignal), { after: "opaque", readerId: "host:reader" });
    expect(screen.getByRole("status")).toHaveTextContent("Reading work… 1 of 2 Engram items so far");
    // Clicks while that read is in flight add nothing.
    const more = screen.getByRole("button", { name: "Load more work" });
    expect(more).toBeDisabled();
    fireEvent.click(more); fireEvent.click(more);
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    const next = page("Second"); next.page!.total = 2; next.page!.shownBefore = 1;
    await act(async () => pending.resolve(next));
    expect(screen.getByRole("button", { name: "w-one — First" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "w-one — Second" })).toBeInTheDocument();
    expect(screen.getByText(/2 of 2 Engram items loaded/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Load more work" })).not.toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(2);
  });

  it("follows continuation cursors until the source has no more", async () => {
    const first = page("A", "c1"); first.page!.total = 3;
    const second = page("B", "c2"); second.page!.total = 3; second.page!.shownBefore = 1;
    const third = page("C"); third.page!.total = 3; third.page!.shownBefore = 2;
    vi.mocked(readProjectWork).mockResolvedValueOnce(first).mockResolvedValueOnce(second).mockResolvedValueOnce(third);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-one — C" });
    expect(screen.getByRole("button", { name: "w-one — A" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "w-one — B" })).toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(3);
    expect(readProjectWork).toHaveBeenNthCalledWith(2, "p-one", expect.any(Object), expect.any(AbortSignal), { after: "c1", readerId: "host:reader" });
    expect(readProjectWork).toHaveBeenNthCalledWith(3, "p-one", expect.any(Object), expect.any(AbortSignal), { after: "c2", readerId: "host:reader" });
    expect(screen.getByText(/3 of 3 Engram items loaded/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Load more work" })).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("stops reading ahead once a page of rows is loaded and leaves the rest to Load more", async () => {
    const full = page("Row 0", "c1");
    full.page!.items = Array.from({ length: WORK_AUTO_LOAD_TARGET_ROWS }, (_, index) => ({ ...full.page!.items[0], id: `row-${index}`, shortRef: `w-${index}`, title: `Row ${index}` }));
    full.page!.total = WORK_AUTO_LOAD_TARGET_ROWS + 50;
    const pending = deferred<WorkListResponse>();
    vi.mocked(readProjectWork).mockResolvedValueOnce(full).mockReturnValueOnce(pending.promise);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: `w-${WORK_AUTO_LOAD_TARGET_ROWS - 1} — Row ${WORK_AUTO_LOAD_TARGET_ROWS - 1}` });
    await act(async () => {});
    expect(readProjectWork).toHaveBeenCalledTimes(1);
    expect(screen.getByText(new RegExp(`${WORK_AUTO_LOAD_TARGET_ROWS} of ${WORK_AUTO_LOAD_TARGET_ROWS + 50} Engram items loaded`))).toBeInTheDocument();
    const more = screen.getByRole("button", { name: "Load more work" });
    expect(more).toBeEnabled();
    fireEvent.click(more);
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    expect(readProjectWork).toHaveBeenLastCalledWith("p-one", expect.any(Object), expect.any(AbortSignal), { after: "c1", readerId: "host:reader" });
  });

  it("stops reading ahead when a continuation makes no progress and counts only Engram rows against the target", async () => {
    // A large Beads snapshot does not count: the Engram page still continues.
    const first = page("First", "c1"); first.page!.total = 2;
    first.beads = { ...beadsPage(), items: Array.from({ length: 300 }, (_, index) => bead(`tm-${index}`, `Bead ${index}`)), total: 300 };
    first.sources.push({ source: "beads", state: "ready", message: "Beads reads use the native bd binary" });
    // The continuation returns the same cursor and no new rows: valid, but
    // not progress. The chain stops instead of re-reading it to the cap.
    const stalled = page("First", "c1"); stalled.page!.items = []; stalled.page!.total = 2; stalled.page!.shownBefore = 1;
    vi.mocked(readProjectWork).mockResolvedValueOnce(first).mockResolvedValue(stalled);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-one — First" });
    await waitFor(() => expect(readProjectWork).toHaveBeenCalledTimes(2));
    await act(async () => {});
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    expect(readProjectWork).toHaveBeenLastCalledWith("p-one", expect.any(Object), expect.any(AbortSignal), { after: "c1", readerId: "host:reader" });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByText(/1 of 2 Engram items loaded · 300 of 300 Beads items loaded; 301 visible/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Load more work" })).toBeEnabled();
  });

  it("dates a retained Beads snapshot to its own read, not to a later Engram page", async () => {
    const first = page("First", "opaque"); first.beads = beadsPage(); first.observedAt = "2026-09-14T10:00:00Z";
    first.sources.push({ source: "beads", state: "ready", message: "Beads reads use the native bd binary" });
    const next = page("Second"); next.page!.total = 2; next.page!.shownBefore = 1; next.observedAt = "2026-09-14T11:00:00Z";
    // The continuation is read automatically; holding it back keeps the
    // first page's caption observable.
    const pending = deferred<WorkListResponse>();
    vi.mocked(readProjectWork).mockResolvedValueOnce(first).mockReturnValueOnce(pending.promise);
    const { container } = render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-one — First" });
    // Times are shown to the minute in the viewer's zone; the raw source value
    // stays on the element, so the assertion names it without assuming a zone.
    const caption = () => container.querySelector(".work-panel-caption:last-of-type")?.textContent;
    expect(caption()).toMatch(/^Read completed: \d{4}-\d{2}-\d{2} \d{2}:\d{2}\. Refresh for current data\.$/);
    expect(screen.getByTitle("2026-09-14T10:00:00Z")).toBeInTheDocument();
    await act(async () => pending.resolve(next));
    await screen.findByRole("button", { name: "w-one — Second" });
    expect(caption()).toMatch(/^Engram read completed: \d{4}-\d{2}-\d{2} \d{2}:\d{2} · Beads snapshot from \d{4}-\d{2}-\d{2} \d{2}:\d{2}\. Refresh for current data\.$/);
    expect(screen.getByTitle("2026-09-14T11:00:00Z")).toBeInTheDocument();
    expect(screen.getByTitle("2026-09-14T10:00:00Z")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "tm-root — Root epic" })).toBeInTheDocument();
  });

  it("keeps a selected kind visible when a refreshed snapshot no longer has it", async () => {
    const first: WorkListResponse = { sources: [{ source: "beads", state: "ready", message: "Beads reads use the native bd binary" }],
      readerId: null, observedAt: "now", page: null,
      beads: { items: [bead("tm-dec", "Decision", { kind: "decision" }), bead("tm-t", "Task")], total: 2, shownBefore: 0, more: false, after: null, hint: null } };
    const second: WorkListResponse = { ...first, beads: { ...first.beads!, items: [bead("tm-t", "Task")], total: 1 } };
    vi.mocked(readProjectWork).mockResolvedValueOnce(first).mockResolvedValueOnce(second);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "tm-dec — Decision" });
    fireEvent.change(screen.getByRole("combobox", { name: "Kind (loaded rows)" }), { target: { value: "decision" } });
    expect(screen.queryByRole("button", { name: "tm-t — Task" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Refresh work" }));
    await waitFor(() => expect(readProjectWork).toHaveBeenCalledTimes(2));
    await screen.findByText(/1 of 1 Beads items loaded/);
    // The kind is gone from the snapshot but not from the filter: the select
    // still shows it and the empty state explains the zero matches.
    expect(screen.getByRole("combobox", { name: "Kind (loaded rows)" })).toHaveValue("decision");
    expect(screen.queryByRole("button", { name: "tm-t — Task" })).not.toBeInTheDocument();
    expect(screen.getByText(/No matching rows in the loaded page/)).toBeInTheDocument();
  });

  it("shows source, waits-for and parent in the table for Beads rows", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({ sources: [{ source: "beads", state: "ready", message: "Beads reads use the native bd binary" }],
      readerId: null, observedAt: "now", page: null, beads: beadsPage() });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "tm-root.1 — Blocked child" });
    showTable();
    expect(screen.getByRole("columnheader", { name: "Source" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Waits for" })).toBeInTheDocument();
    expect(screen.getAllByRole("cell", { name: "beads" })).toHaveLength(3);
    // Prerequisites as the source reported them, satisfied ones marked; the
    // parent is shown with the reference, never inferred from the id.
    expect(screen.getByRole("cell", { name: "tm-free, tm-closed (satisfied)" })).toBeInTheDocument();
    expect(screen.getByText("in tm-root")).toBeInTheDocument();
  });

  it("renders disabled and unavailable sources without claiming the project is empty", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({ sources: [{ source: "engram", state: "disabled", message: "Not enabled by operator" }, { source: "beads", state: "absent", message: "No .beads directory" }], page: null, beads: null, readerId: null, observedAt: "now" });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    expect(await screen.findByText(/Not enabled by operator/)).toBeInTheDocument();
    expect(screen.getByText(/No \.beads directory/)).toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
    expect(screen.queryByText(/No work items match/)).not.toBeInTheDocument();
    expect(screen.queryByText(/items loaded/)).not.toBeInTheDocument();
  });

  it("nests loaded Beads rows under the goals they block and keeps the hierarchy separate", async () => {
    const response = page(); response.beads = beadsPage();
    response.sources.push({ source: "beads", state: "ready", message: "Beads reads use the native bd binary" });
    vi.mocked(readProjectWork).mockResolvedValue(response);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    const goal = (await screen.findByRole("button", { name: "tm-root.1 — Blocked child" })).closest("li")!;
    expect(screen.getByText("Blocked chains · 1 goal")).toBeInTheDocument();
    expect(within(goal).getByRole("button", { name: "tm-free — Ready bug" })).toBeInTheDocument();
    expect(within(goal).getByText("1 satisfied · not loaded")).toBeInTheDocument();
    expect(within(goal).queryByText(/waits for \d+ not loaded/)).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Dependency tree" })).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "tm-free — Ready bug" })).toHaveLength(1);
    expect(screen.getByText("No visible dependency links · 2 items")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "w-one — Visible task" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "tm-root — Root epic" })).toBeInTheDocument();
    expect(screen.getByText(/1 of 1 Engram items loaded · 3 of 3 Beads items loaded; 4 visible/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Collapse tm-root.1" }));
    expect(screen.queryByRole("button", { name: "tm-free — Ready bug" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Expand tm-root.1" }));
    expect(screen.getByRole("button", { name: "tm-free — Ready bug" })).toBeInTheDocument();

    fireEvent.change(screen.getByRole("combobox", { name: "Kind (loaded rows)" }), { target: { value: "task" } });
    expect(screen.getByText("1 hidden by filter")).toBeInTheDocument();
    fireEvent.change(screen.getByRole("combobox", { name: "Kind (loaded rows)" }), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "Hierarchy" }));
    const parent = screen.getByRole("button", { name: "tm-root — Root epic" }).closest("li")!;
    expect(within(parent).getByRole("button", { name: "tm-root.1 — Blocked child" })).toBeInTheDocument();
    expect(within(parent).queryByRole("button", { name: "tm-free — Ready bug" })).not.toBeInTheDocument();
    expect(screen.getByText(/3 roots/)).toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(1);
  });

  it("reads Beads details on demand without an Engram reader and renders relations as inert data", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({ sources: [{ source: "engram", state: "disabled", message: "Not enabled by operator" }, { source: "beads", state: "ready", message: "Beads reads use the native bd binary" }],
      readerId: null, observedAt: "now", page: null, beads: beadsPage() });
    vi.mocked(readWorkBeadsDetail).mockResolvedValue({ item: bead("tm-root.1", "Blocked child", { parentId: "tm-root", availability: "blocked", priority: 1 }), description: "Waits for <b>inert</b>", parent: "tm-root",
      dependencies: [{ id: "tm-free", title: "Ready bug", status: "open", priority: 3, kind: "bug", dependencyType: "blocks" }], dependenciesUnread: 0, dependentCount: 0,
      comments: [{ id: "c-1", author: "Greg", text: "First <i>inert</i>", createdAt: "2026-09-12T10:00:00Z" }], commentCount: 1, observedAt: "later" });
    const { container } = render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    const row = await screen.findByRole("button", { name: "tm-root.1 — Blocked child" });
    fireEvent.click(row);
    const details = await screen.findByRole("complementary", { name: "Work item details" });
    expect(details).toHaveFocus();
    expect(await within(details).findByText("Waits for <b>inert</b>")).toBeInTheDocument();
    expect(readWorkBeadsDetail).toHaveBeenCalledWith("p-one", "tm-root.1", expect.any(AbortSignal));
    expect(readWorkDetail).not.toHaveBeenCalled();
    expect(within(details).getByText("blocks")).toBeInTheDocument();
    expect(within(details).getByText("tm-free — Ready bug")).toBeInTheDocument();
    expect(within(details).getByText("First <i>inert</i>")).toBeInTheDocument();
    expect(within(details).getByText("Subtask of tm-root")).toBeInTheDocument();
    expect(container.querySelector("b, i, script")).toBeNull();
    expect(row).toHaveAttribute("aria-current", "true");
    fireEvent.keyDown(details, { key: "Escape" });
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
    expect(row).toHaveFocus();
  });

  it("keeps the Beads snapshot and its status across an Engram continuation", async () => {
    const first = page("First", "opaque"); first.beads = beadsPage();
    first.sources.push({ source: "beads", state: "ready", message: "Beads reads use the native bd binary" });
    const next = page("Second"); next.page!.total = 2; next.page!.shownBefore = 1;
    next.sources.push({ source: "beads", state: "ready", message: "second detection" });
    vi.mocked(readProjectWork).mockResolvedValueOnce(first).mockResolvedValueOnce(next);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    // The continuation is read automatically after the first page.
    await screen.findByRole("button", { name: "w-one — Second" });
    expect(screen.getByRole("button", { name: "tm-root — Root epic" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "w-one — First" })).toBeInTheDocument();
    expect(screen.getByText(/2 of 2 Engram items loaded · 3 of 3 Beads items loaded; 5 visible/)).toBeInTheDocument();
    expect(screen.getByText(/Beads reads use the native bd binary/)).toBeInTheDocument();
    expect(screen.queryByText(/second detection/)).not.toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(2);
  });

  it("keeps the Beads snapshot when an Engram continuation is rejected", async () => {
    const first = page("First", "opaque"); first.beads = beadsPage();
    first.sources.push({ source: "beads", state: "ready", message: "Beads reads use the native bd binary" });
    // The continuation is read automatically; holding its rejection back lets
    // the drawer open on the first generation before that generation drops.
    let reject!: (error: unknown) => void;
    const pending = new Promise<WorkListResponse>((_, fail) => { reject = fail; });
    vi.mocked(readProjectWork).mockResolvedValueOnce(first).mockReturnValueOnce(pending);
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    fireEvent.click(await screen.findByRole("button", { name: "w-one — First" }));
    const details = await screen.findByRole("complementary", { name: "Work item details" });
    expect(details).toHaveFocus();
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    await act(async () => { reject(new ApiRequestError("request-failed", "work_catalog_cursor_invalid", { status: 409 })); });
    expect(await screen.findByRole("alert")).toHaveTextContent("work_catalog_cursor_invalid");
    // The drawer that held the focus is gone with its generation: focus is
    // parked on the list, never dropped on the body.
    expect(document.activeElement).toHaveClass("work-panel-list");
    expect(screen.queryByRole("button", { name: /w-one/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "tm-root — Root epic" })).toBeInTheDocument();
    expect(screen.getByText(/engram: error/)).toBeInTheDocument();
    expect(screen.getByText(/Beads reads use the native bd binary/)).toBeInTheDocument();
    expect(screen.getByText(/3 of 3 Beads items loaded; 3 visible/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Load more work" })).not.toBeInTheDocument();
  });

  it("shows a Beads detail failure as an alert and re-reads on Reload details", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({ sources: [{ source: "beads", state: "ready", message: "Beads reads use the native bd binary" }],
      readerId: null, observedAt: "now", page: null, beads: beadsPage() });
    vi.mocked(readWorkBeadsDetail)
      .mockRejectedValueOnce(new ApiRequestError("request-failed", "Beads issue tm-free not found", { status: 404 }))
      .mockResolvedValueOnce({ item: bead("tm-free", "Ready bug"), description: "Back again", parent: null, dependencies: [], dependenciesUnread: 0, dependentCount: 0, comments: [], commentCount: 0, observedAt: "later" });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    fireEvent.click(await screen.findByRole("button", { name: "tm-free — Ready bug" }));
    const details = await screen.findByRole("complementary", { name: "Work item details" });
    expect(await within(details).findByRole("alert")).toHaveTextContent("Beads issue tm-free not found");
    expect(within(details).queryByRole("heading", { level: 3 })).not.toBeInTheDocument();
    fireEvent.click(within(details).getByRole("button", { name: "Reload details" }));
    expect(await within(details).findByText("Back again")).toBeInTheDocument();
    expect(within(details).queryByRole("alert")).not.toBeInTheDocument();
    expect(readWorkBeadsDetail).toHaveBeenCalledTimes(2);
  });

  it("returns focus to the list when the selected row's button is no longer mounted", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({ sources: [{ source: "beads", state: "ready", message: "Beads reads use the native bd binary" }],
      readerId: null, observedAt: "now", page: null, beads: beadsPage() });
    vi.mocked(readWorkBeadsDetail).mockResolvedValue({ item: bead("tm-free", "Ready bug"), description: "", parent: null, dependencies: [], dependenciesUnread: 0, dependentCount: 1, comments: [], commentCount: 0, observedAt: "now" });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    fireEvent.click(await screen.findByRole("button", { name: "tm-free — Ready bug" }));
    const details = await screen.findByRole("complementary", { name: "Work item details" });
    fireEvent.click(screen.getByRole("button", { name: "Collapse tm-root.1" }));
    expect(screen.queryByRole("button", { name: "tm-free — Ready bug" })).not.toBeInTheDocument();
    fireEvent.keyDown(details, { key: "Escape" });
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
    expect(document.activeElement).toHaveClass("work-panel-list");
  });

  it("never claims an empty tracker while a source failed", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({
      sources: [{ source: "engram", state: "error", message: "engram work ls: exit code: 1: database is locked" }, { source: "beads", state: "ready", message: "Beads reads use the native bd binary" }],
      readerId: null, observedAt: "now", page: null,
      beads: { items: [], total: 0, shownBefore: 0, more: false, after: null, hint: null },
    });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    expect(await screen.findByText(/not a complete tracker view/)).toBeInTheDocument();
    expect(screen.queryByText(/No work items match/)).not.toBeInTheDocument();
    expect(screen.getByText(/engram: error/)).toBeInTheDocument();
  });

  it("shows a Beads read failure as an explicit source error next to the Engram rows", async () => {
    const response = page();
    response.sources.push({ source: "beads", state: "error", message: "bd list: database is locked" });
    vi.mocked(readProjectWork).mockResolvedValue(response);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    expect(await screen.findByRole("button", { name: "w-one — Visible task" })).toBeInTheDocument();
    expect(screen.getByText(/database is locked/)).toBeInTheDocument();
    expect(screen.getByText(/1 of 1 Engram items loaded; 1 visible/)).toBeInTheDocument();
    expect(screen.queryByText(/Beads items loaded/)).not.toBeInTheDocument();
  });

  it("applies source filters only on submit and labels local filtering as loaded-row filtering", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-one — Visible task" });
    fireEvent.change(screen.getByRole("textbox", { name: "Search" }), { target: { value: "a query" } });
    expect(readProjectWork).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "Apply filters" }));
    await waitFor(() => expect(readProjectWork).toHaveBeenCalledTimes(2));
    fireEvent.change(await screen.findByRole("combobox", { name: "Kind (loaded rows)" }), { target: { value: "task" } });
    expect(screen.queryByRole("button", { name: /Visible task/ })).not.toBeInTheDocument();
    expect(screen.getByText(/1 of 1 Engram items loaded; 0 visible/)).toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(2);
  });

  it("renders receipt text as inert data and reads details only on demand", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page("<script>claim()</script>"));
    vi.mocked(readWorkDetail).mockResolvedValue({ status: { work: { shortRef: "w-one", title: "Details", outcome: "<b>not markup</b>", acceptance: [], acceptanceOmitted: null, lifecycle: "open", priority: 1, kind: "bug" }, availability: "blocked" }, holder: "peer-label", heldUntil: null, notes: [], notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
    const { container } = render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    fireEvent.click(await screen.findByRole("button", { name: "w-one — <script>claim()</script>" }));
    expect(await screen.findByText("<b>not markup</b>")).toBeInTheDocument();
    expect(container.querySelector("script")).toBeNull();
    expect(screen.queryByRole("link", { name: "peer-label" })).not.toBeInTheDocument();
    expect(readWorkDetail).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("complementary", { name: "Work item details" })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "w-one — <script>claim()</script>" })).toHaveFocus();
  });

  it("keeps detail identity while loading older notes and discards an invalid window", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    const detail: WorkDetailResponse = {
      status: { work: { shortRef: "w-one", title: "Detail title", outcome: "Preserved outcome", acceptance: [], acceptanceOmitted: null, lifecycle: "open", priority: 1, kind: "bug" }, availability: "blocked" },
      holder: "peer-label", heldUntil: null,
      notes: [{ locator: "newest", summary: "Newer note", kind: "note", family: "notes", by: null, createdAt: "now", bodyOmitted: false, summaryTruncated: false }],
      notesWindow: { total: 3, shown: 1, newer: 0, older: 2, after: "first-cut", readCut: { projectPosition: 3, observedAt: "now", validUntilMs: 123 } },
    };
    const continuation: WorkDetailResponse = { ...detail, status: null, holder: null,
      notes: [{ ...detail.notes[0], locator: "older", summary: "Older note" }],
      notesWindow: { ...detail.notesWindow, newer: 1, older: 1, after: "second-cut", readCut: { projectPosition: 3, observedAt: "later", validUntilMs: 456 } },
    };
    vi.mocked(readWorkDetail).mockResolvedValueOnce(detail).mockResolvedValueOnce(continuation)
      .mockRejectedValueOnce(new ApiRequestError("request-failed", "work_show_cursor_invalid", { status: 409 }));
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    fireEvent.click(await screen.findByRole("button", { name: "w-one — Visible task" }));
    fireEvent.click(await screen.findByRole("button", { name: "Load older notes" }));
    expect(await screen.findByText("Older note")).toBeInTheDocument();
    expect(screen.getByText("Preserved outcome")).toBeInTheDocument();
    expect(screen.getByText("Holder: peer-label")).toBeInTheDocument();
    expect(readWorkDetail).toHaveBeenLastCalledWith("p-one", "w-one", "host:reader", expect.any(AbortSignal), "first-cut");
    fireEvent.click(screen.getByRole("button", { name: "Load older notes" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("work_show_cursor_invalid");
    expect(screen.queryByText("Older note")).not.toBeInTheDocument();
    expect(screen.queryByText("Preserved outcome")).not.toBeInTheDocument();
  });

  it("preserves owner/peer provenance, inert refs and newest-first order across multi-note windows", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    const note = (id: number): WorkDetailResponse["notes"][number] => ({ locator: `note-${id}`, summary: `Summary ${id}`, kind: "status", family: "observations", by: "same label", createdAt: `time-${id}`, bodyOmitted: false, summaryTruncated: false });
    const detail: WorkDetailResponse = { status: null, holder: null, heldUntil: null,
      notes: [{ ...note(3), statusOwner: false, nonHolder: true, refs: ["<script>do_not_run()</script>"] }, { ...note(4), statusOwner: true, refs: ["report/path"] }],
      notesWindow: { total: 4, shown: 2, newer: 0, older: 2, after: "opaque", readCut: { projectPosition: 7, observedAt: "first", validUntilMs: null } } };
    vi.mocked(readWorkDetail).mockResolvedValueOnce(detail).mockResolvedValueOnce({ ...detail,
      notes: [note(1), note(2)], notesWindow: { ...detail.notesWindow, newer: 2, older: 0, after: null,
        readCut: { ...detail.notesWindow.readCut, observedAt: "later" } } });
    const { container } = render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    fireEvent.click(await screen.findByRole("button", { name: "w-one — Visible task" }));
    const owner = (await screen.findByText("Summary 4")).closest("li")!;
    const peer = screen.getByText("Summary 3").closest("li")!;
    expect(within(owner).getByText("Owner status commitment.")).toBeInTheDocument();
    expect(within(peer).getByText("Peer status observation, no commitment.")).toBeInTheDocument();
    expect(within(peer).getByText("Recorded by a non-holder.")).toBeInTheDocument();
    expect(within(peer).getByText("<script>do_not_run()</script>")).toBeInTheDocument();
    expect(screen.getByText("report/path")).toBeInTheDocument();
    expect(container.querySelector("script")).toBeNull();
    expect(screen.queryByRole("link")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Load older notes" }));
    await screen.findByText("Summary 1");
    expect(screen.getAllByText(/^Summary /).map(node => node.textContent)).toEqual(["Summary 4", "Summary 3", "Summary 2", "Summary 1"]);
    expect(screen.getAllByText("Status ownership not reported by source.")).toHaveLength(2);
    expect(screen.getAllByText("References not included by source.")).toHaveLength(2);
  });
});
