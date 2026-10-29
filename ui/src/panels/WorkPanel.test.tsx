import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { ApiRequestError } from "../api-request";
import { readProjectWork, readWorkDetail, type WorkItem, type WorkListResponse, type WorkDetailResponse, type WorkPage } from "../work-visualizer-api";
import { WorkPanel } from "./WorkPanel";
import { WorkDetailsHeader } from "./WorkDetailsHeader";

vi.mock("../work-visualizer-api", () => ({ readProjectWork: vi.fn(), readWorkDetail: vi.fn() }));
const projects = [
  { id: "p-one", name: "One", rootPath: "/one", remoteId: "local" },
  { id: "p-two", name: "Two", rootPath: "/two", remoteId: "local" },
] as Project[];
function page(title = "Visible task", after: string | null = null): WorkListResponse {
  return { sources: [{ source: "engram", state: "ready", message: "Engram reads use the host reader over the validated project store" }], readerId: "host:reader", observedAt: "now",
    page: { items: [{ id: title, shortRef: "w-one", title, kind: "bug", lifecycle: "open", availability: "blocked", priority: 1, labels: ["decision"], assignedTo: "actor", parentId: null, updatedAt: "today", blockedBy: [], source: "engram", prerequisites: [] }], total: after ? 2 : 1, shownBefore: 0, more: !!after, after, hint: null } };
}
function item(id: string, title: string, extra: Partial<WorkItem> = {}): WorkItem {
  return { id, shortRef: id, title, kind: "task", lifecycle: "open", availability: "ready", priority: 2, labels: [], assignedTo: null, parentId: null, updatedAt: "today", blockedBy: [], source: "engram", prerequisites: [], ...extra };
}
// Mirrors the Rust fixture: a goal blocked by one loaded and one absent
// prerequisite, its parent, and the ready item it waits for.
function fixturePage(): WorkPage {
  return { items: [
    item("tm-root", "Root epic", { kind: "feature", availability: "active", assignedTo: "Termal::Codex" }),
    item("tm-root.1", "Blocked child", { parentId: "tm-root", availability: "blocked", priority: 1, prerequisites: [{ id: "tm-free", satisfied: false }, { id: "tm-closed", satisfied: true }] }),
    item("tm-free", "Ready bug", { kind: "bug", priority: 3 }),
  ], total: 3, shownBefore: 0, more: false, after: null, hint: null };
}
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(r => { resolve = r; }); return { promise, resolve }; }
function showTable() { fireEvent.click(screen.getByRole("button", { name: "Table" })); }

describe("WorkPanel", () => {
  beforeEach(() => { vi.resetAllMocks(); localStorage.clear(); });
  afterEach(() => vi.unstubAllGlobals());

  it("keeps the compact detail icons named and wired to their actions", () => {
    const onClose = vi.fn();
    const onReload = vi.fn();
    render(<WorkDetailsHeader workRef="w-one" onClose={onClose} onReload={onReload} />);
    for (const [name, callback] of [["Reload details", onReload], ["Close details", onClose]] as const) {
      const button = screen.getByRole("button", { name });
      expect(button).toHaveAttribute("title", name);
      expect(button).toHaveTextContent("");
      expect(button.querySelector('svg[aria-hidden="true"]')).not.toBeNull();
      fireEvent.click(button);
      expect(callback).toHaveBeenCalledTimes(1);
    }
  });

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

  it("boxes the bold ID after priority and starts labels in the same content column", async () => {
    const response = page();
    response.page!.items[0]!.shortRef = "w-94643039d5a7";
    vi.mocked(readProjectWork).mockResolvedValue(response);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-94643039d5a7 — Visible task" });
    for (const view of ["Dependencies", "Hierarchy"]) {
      fireEvent.click(screen.getByRole("button", { name: view }));
      const title = screen.getByRole("button", { name: "w-94643039d5a7 — Visible task" });
      expect(title).toHaveTextContent(/^Visible task$/);
      const ref = title.previousElementSibling!;
      expect(ref.tagName).toBe("STRONG");
      expect(ref).toHaveClass("work-row-ref");
      expect(ref.textContent).toBe("w-94643039d5a7");
      const content = title.closest(".work-node-content")!;
      expect(content.previousElementSibling).toHaveClass("work-priority");
      expect(content.children[0]).toHaveClass("work-node-row");
      expect(content.children[1]).toHaveClass("work-row-secondary");
      expect(content.children[1].querySelector(".work-label-chips")).toBeInTheDocument();
      expect(within(content as HTMLElement).getByRole("button", { name: "Filter by label decision" })).toBeInTheDocument();
    }
    showTable();
    expect(screen.getByRole("button", { name: "w-94643039d5a7 — Visible task" })).toHaveTextContent("w-94643039d5a7 — Visible task");
  });

  it("keeps the chosen view and sort across local-filter submissions", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-one — Visible task" });
    showTable();
    fireEvent.change(screen.getByRole("combobox", { name: "Sort (loaded rows)" }), { target: { value: "title" } });
    // Submitting display filters keeps the existing snapshot and presentation.
    fireEvent.click(screen.getByRole("button", { name: "Apply filters" }));
    await act(async () => {});
    expect(readProjectWork).toHaveBeenCalledTimes(1);
    await screen.findByRole("button", { name: "w-one — Visible task" });
    expect(screen.getByRole("button", { name: "Table", pressed: true })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Work table scroll area" })).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Sort (loaded rows)" })).toHaveValue("title");
  });

  it("sorts the table from a clicked header, reverses on the second click and shares one state with the sort control", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({ sources: [{ source: "engram", state: "ready", message: "Ready" }], readerId: "host:reader", observedAt: "now", page: fixturePage() });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "tm-root — Root epic" });
    showTable();
    const column = (index: number) => within(screen.getByRole("table")).getAllByRole("row").slice(1).map(row => within(row).getAllByRole("cell")[index]!.textContent);
    const header = (name: string) => screen.getByRole("columnheader", { name });
    // Priority first until a header is chosen; every header is a button, so it
    // is reachable from the keyboard, and reports its state through aria-sort.
    expect(column(3)).toEqual(["task", "feature", "bug"]);
    expect(header("Priority")).toHaveAttribute("aria-sort", "ascending");
    expect(header("Kind")).toHaveAttribute("aria-sort", "none");
    expect(screen.getByRole("combobox", { name: "Sort (loaded rows)" })).toHaveValue("priority");
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

  it("leads single-source tree rows with priority coloured by availability and omits source glyphs", async () => {
    const loaded = page("Engram row"); loaded.page!.items.push(...fixturePage().items); loaded.page!.total = 4;
    vi.mocked(readProjectWork).mockResolvedValueOnce(loaded);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    const engramRow = await screen.findByRole("button", { name: "w-one — Engram row" });
    // The lead precedes the title in reading order; the availability word is
    // there for assistive technology and on hover, the colour comes from it.
    const lead = engramRow.closest(".work-node-heading")!.querySelector(".work-priority")!;
    expect(lead).toHaveClass("work-priority");
    expect(lead).toHaveAttribute("data-state", "blocked");
    expect(lead).toHaveAttribute("title", "blocked");
    expect(lead).toHaveTextContent("P1, blocked");
    expect(lead.compareDocumentPosition(engramRow) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    const freeRow = screen.getByRole("button", { name: "tm-free — Ready bug" });
    expect(freeRow.closest(".work-node-heading")!.querySelector(".work-priority")).toHaveAttribute("data-state", "ready");
    expect(screen.queryByText("engram")).not.toBeInTheDocument();
    expect(screen.queryByText("ready")).not.toBeInTheDocument();
    expect(screen.queryByRole("img", { name: /^Source: / })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "tm-root.1 — Blocked child" }).closest(".work-node-heading")!.querySelector(".work-priority")).toHaveAttribute("data-state", "blocked");
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

  it("Refresh starts a new generation after an error and aborts the previous request", async () => {
    vi.mocked(readProjectWork).mockRejectedValueOnce(new ApiRequestError("request-failed", "Work reads busy", { status: 429 }))
      .mockResolvedValueOnce(page("Retried"));
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("alert");
    const firstSignal = vi.mocked(readProjectWork).mock.calls[0][2];
    fireEvent.click(screen.getByRole("button", { name: "Refresh work" }));
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
    // Scroll events are not page demand and cannot overlap the serial read.
    fireEvent.scroll(screen.getByRole("region", { name: "Work list" }));
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

  it("continues serially beyond the former 200-row prefetch target without a click", async () => {
    const loadedRows = 200;
    const full = page("Row 0", "c1");
    full.page!.items = Array.from({ length: loadedRows }, (_, index) => ({ ...full.page!.items[0], id: `row-${index}`, shortRef: `w-${index}`, title: `Row ${index}` }));
    full.page!.total = loadedRows + 50;
    const pending = deferred<WorkListResponse>();
    vi.mocked(readProjectWork).mockResolvedValueOnce(full).mockReturnValueOnce(pending.promise);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-0 — Row 0" });
    await act(async () => {});
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    expect(screen.getByText(new RegExp(`${loadedRows} of ${loadedRows + 50} Engram items loaded`))).toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    expect(readProjectWork).toHaveBeenLastCalledWith("p-one", expect.any(Object), expect.any(AbortSignal), { after: "c1", readerId: "host:reader" });
  });

  it("stops reading ahead when an Engram continuation makes no progress", async () => {
    const first = page("First", "c1"); first.page!.total = 2;
    // An unchanged cursor and no new rows pauses until an explicit retry.
    const stalled = page("First", "c1"); stalled.page!.items = []; stalled.page!.total = 2; stalled.page!.shownBefore = 1;
    vi.mocked(readProjectWork).mockResolvedValueOnce(first).mockResolvedValue(stalled);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-one — First" });
    await waitFor(() => expect(readProjectWork).toHaveBeenCalledTimes(2));
    await act(async () => {});
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    expect(readProjectWork).toHaveBeenLastCalledWith("p-one", expect.any(Object), expect.any(AbortSignal), { after: "c1", readerId: "host:reader" });
    expect(screen.getByRole("alert")).toHaveTextContent("no progress");
    expect(screen.getByText(/1 of 2 Engram items loaded; 1 visible/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry" })).toBeEnabled();
  });

  it("keeps a selected kind visible when a refreshed snapshot no longer has it", async () => {
    const first: WorkListResponse = { sources: [{ source: "engram", state: "ready", message: "Ready" }],
      readerId: "host:reader", observedAt: "now", page: { items: [item("tm-dec", "Decision", { kind: "decision" }), item("tm-t", "Task")], total: 2, shownBefore: 0, more: false, after: null, hint: null } };
    const second: WorkListResponse = { ...first, page: { ...first.page!, items: [item("tm-t", "Task")], total: 1 } };
    vi.mocked(readProjectWork).mockResolvedValueOnce(first).mockResolvedValueOnce(second);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "tm-dec — Decision" });
    fireEvent.change(screen.getByRole("combobox", { name: "Kind (loaded rows)" }), { target: { value: "decision" } });
    expect(screen.queryByRole("button", { name: "tm-t — Task" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Refresh work" }));
    await waitFor(() => expect(readProjectWork).toHaveBeenCalledTimes(2));
    await screen.findByText(/1 of 1 Engram items loaded/);
    // The kind is gone from the snapshot but not from the filter: the select
    // still shows it and the empty state explains the zero matches.
    expect(screen.getByRole("combobox", { name: "Kind (loaded rows)" })).toHaveValue("decision");
    expect(screen.queryByRole("button", { name: "tm-t — Task" })).not.toBeInTheDocument();
    expect(screen.getByText(/No matching rows in the loaded page/)).toBeInTheDocument();
  });

  it("shows source, waits-for and parent in the table for Engram rows", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({ sources: [{ source: "engram", state: "ready", message: "Ready" }],
      readerId: "host:reader", observedAt: "now", page: fixturePage() });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "tm-root.1 — Blocked child" });
    showTable();
    expect(screen.getByRole("columnheader", { name: "Source" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Waits for" })).toBeInTheDocument();
    expect(screen.getAllByRole("cell", { name: "engram" })).toHaveLength(3);
    // Prerequisites as the source reported them, satisfied ones marked; the
    // parent is shown with the reference, never inferred from the id.
    expect(screen.getByRole("cell", { name: "tm-free, tm-closed (satisfied)" })).toBeInTheDocument();
    expect(screen.getByText("in tm-root")).toBeInTheDocument();
  });

  it("renders disabled and unavailable sources without claiming the project is empty", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({ sources: [{ source: "engram", state: "disabled", message: "Not enabled by operator" }], page: null, readerId: null, observedAt: "now" });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    expect(await screen.findByText(/Not enabled by operator/)).toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
    expect(screen.queryByText(/No work items match/)).not.toBeInTheDocument();
    expect(screen.queryByText(/items loaded/)).not.toBeInTheDocument();
  });

  it("nests loaded Engram rows under the goals they block and keeps the hierarchy separate", async () => {
    const response = page(); response.page!.items.push(...fixturePage().items); response.page!.total = 4;
    vi.mocked(readProjectWork).mockResolvedValue(response);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    const goal = (await screen.findByRole("button", { name: "tm-root.1 — Blocked child" })).closest("li")!;
    expect(screen.getByText("Blocked chains · 1 goal")).toBeInTheDocument();
    expect(goal).toHaveAttribute("aria-level", "1");
    expect(screen.getByRole("button", { name: "tm-free — Ready bug" }).closest("li")).toHaveAttribute("aria-level", "2");
    expect(within(goal).getByText("1 satisfied · not loaded")).toBeInTheDocument();
    expect(within(goal).queryByText(/waits for \d+ not loaded/)).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Dependency tree" })).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "tm-free — Ready bug" })).toHaveLength(1);
    expect(screen.getByText("No visible dependency links · 2 items")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "w-one — Visible task" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "tm-root — Root epic" })).toBeInTheDocument();
    expect(screen.getByText(/4 of 4 Engram items loaded; 4 visible/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Collapse tm-root.1" }));
    expect(screen.queryByRole("button", { name: "tm-free — Ready bug" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Expand tm-root.1" }));
    expect(screen.getByRole("button", { name: "tm-free — Ready bug" })).toBeInTheDocument();

    fireEvent.change(screen.getByRole("combobox", { name: "Kind (loaded rows)" }), { target: { value: "task" } });
    expect(screen.getByText("1 hidden by filter")).toBeInTheDocument();
    fireEvent.change(screen.getByRole("combobox", { name: "Kind (loaded rows)" }), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "Hierarchy" }));
    const parent = screen.getByRole("button", { name: "tm-root — Root epic" }).closest("li")!;
    expect(parent).toHaveAttribute("aria-level", "1");
    expect(screen.getByRole("button", { name: "tm-root.1 — Blocked child" }).closest("li")).toHaveAttribute("aria-level", "2");
    expect(screen.getByRole("button", { name: "tm-free — Ready bug" }).closest("li")).toHaveAttribute("aria-level", "1");
    expect(screen.getByText(/3 roots/)).toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(1);
  });

  it("keeps loaded Engram rows across a continuation", async () => {
    const first = page("First", "opaque");
    const next = page("Second"); next.page!.total = 2; next.page!.shownBefore = 1;
    vi.mocked(readProjectWork).mockResolvedValueOnce(first).mockResolvedValueOnce(next);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    // The continuation is read automatically after the first page.
    await screen.findByRole("button", { name: "w-one — Second" });
    expect(screen.getByRole("button", { name: "w-one — First" })).toBeInTheDocument();
    expect(screen.getByText(/2 of 2 Engram items loaded; 2 visible/)).toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(2);
  });

  it("closes details and restores list focus when an Engram continuation is rejected", async () => {
    const first = page("First", "opaque");
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
    expect(screen.getByText(/engram: error/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Load more work" })).not.toBeInTheDocument();
  });

  it("closes inline details and focuses the ancestor control when its branch collapses", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({ sources: [{ source: "engram", state: "ready", message: "Ready" }],
      readerId: "host:reader", observedAt: "now", page: fixturePage() });
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    fireEvent.click(await screen.findByRole("button", { name: "tm-free — Ready bug" }));
    await screen.findByRole("complementary", { name: "Work item details" });
    const collapse = screen.getByRole("button", { name: "Collapse tm-root.1" });
    fireEvent.click(collapse);
    expect(screen.queryByRole("button", { name: "tm-free — Ready bug" })).not.toBeInTheDocument();
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
    expect(collapse).toHaveFocus();
    fireEvent.click(collapse);
    expect(screen.getByRole("button", { name: "tm-free — Ready bug" })).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
  });

  it.each(["Dependencies", "Hierarchy", "Labels", "Table"])("expands details under the selected task in %s and toggles them closed", async view => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-one — Visible task" });
    fireEvent.click(screen.getByRole("button", { name: view }));
    const title = screen.getByRole("button", { name: "w-one — Visible task" });
    fireEvent.click(title);
    const details = await screen.findByRole("complementary", { name: "Work item details" });
    expect(title).toHaveAttribute("aria-expanded", "true");
    if (view === "Table") {
      expect(title.closest("tr")!.nextElementSibling).toContainElement(details);
      expect(details.closest("td")).toHaveAttribute("colspan", "9");
    } else {
      expect(title.closest("li")).toContainElement(details);
      expect(title.compareDocumentPosition(details) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    }
    expect(details.closest(".work-panel-list")).not.toBeNull();
    fireEvent.click(title);
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
    expect(title).toHaveAttribute("aria-expanded", "false");
    expect(title).toHaveFocus();
  });

  it.each(["Hierarchy", "Table", "Labels"])("keeps visible details on a switch to %s without stealing the view button's focus", async view => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    fireEvent.click(await screen.findByRole("button", { name: "w-one — Visible task" }));
    await screen.findByRole("complementary");
    const control = screen.getByRole("button", { name: view });
    control.focus();
    fireEvent.click(control);
    expect(screen.getByRole("complementary")).toBeInTheDocument();
    expect(control).toHaveFocus();
    // Presentation changes retain the list; details use the same reader.
    expect(readProjectWork).toHaveBeenCalledTimes(1);
    const back = screen.getByRole("button", { name: "Dependencies" });
    back.focus();
    fireEvent.click(back);
    expect(screen.getByRole("complementary")).toBeInTheDocument();
    expect(back).toHaveFocus();
  });

  it("keeps focus on the Kind control when its filter removes the expanded item", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    fireEvent.click(await screen.findByRole("button", { name: "w-one — Visible task" }));
    await screen.findByRole("complementary");
    const control = screen.getByRole("combobox", { name: "Kind (loaded rows)" });
    control.focus();
    fireEvent.change(control, { target: { value: "task" } });
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
    expect(control).toHaveFocus();
    fireEvent.change(control, { target: { value: "" } });
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
  });

  it("collapses all labels without focusing the selected or previously selected title", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-one — Visible task" });
    fireEvent.click(screen.getByRole("button", { name: "Labels" }));
    const title = screen.getByRole("button", { name: "w-one — Visible task" });
    fireEvent.click(title);
    await screen.findByRole("complementary");
    const focus = vi.spyOn(title, "focus");
    const collapse = screen.getByRole("button", { name: "Collapse all labels" });
    collapse.focus();
    fireEvent.click(collapse);
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
    expect(focus).not.toHaveBeenCalled();
    expect(collapse).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "Expand all labels" }));
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
    fireEvent.click(collapse);
    expect(focus).not.toHaveBeenCalled();
    expect(collapse).toHaveFocus();
    focus.mockRestore();
  });

  it("expands only the clicked label occurrence and closes it when the group collapses", async () => {
    const loaded = page();
    loaded.page!.items[0]!.labels = ["decision", "ui"];
    vi.mocked(readProjectWork).mockResolvedValue(loaded);
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-one — Visible task" });
    fireEvent.click(screen.getByRole("button", { name: "Labels" }));
    const first = screen.getByRole("region", { name: "Label group: decision" });
    const second = screen.getByRole("region", { name: "Label group: ui" });
    const title = within(second).getByRole("button", { name: "w-one — Visible task" });
    fireEvent.click(title);
    expect(await within(second).findByRole("complementary")).toBeInTheDocument();
    expect(within(first).queryByRole("complementary")).not.toBeInTheDocument();
    expect(within(first).getByRole("button", { name: "w-one — Visible task" })).toHaveAttribute("aria-expanded", "false");
    expect(readWorkDetail).toHaveBeenCalledTimes(1);
    fireEvent.click(within(first).getByRole("button", { name: "w-one — Visible task" }));
    expect(await within(first).findByRole("complementary")).toBeInTheDocument();
    expect(within(second).queryByRole("complementary")).not.toBeInTheDocument();
    const collapse = within(first).getByRole("button", { name: /decision.*1/, expanded: true });
    fireEvent.click(collapse);
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
    expect(collapse).toHaveFocus();
    fireEvent.click(collapse);
    expect(screen.queryByRole("complementary")).not.toBeInTheDocument();
  });

  it("never claims an empty tracker while a source failed", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({
      sources: [{ source: "engram", state: "error", message: "engram work ls: exit code: 1: database is locked" }],
      readerId: null, observedAt: "now", page: null,
    });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    expect(await screen.findByText(/not a complete tracker view/)).toBeInTheDocument();
    expect(screen.queryByText(/No work items match/)).not.toBeInTheDocument();
    expect(screen.getByText(/engram: error/)).toBeInTheDocument();
  });

  it("applies search locally on submit and labels filtering as loaded-row filtering", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("button", { name: "w-one — Visible task" });
    fireEvent.change(screen.getByRole("textbox", { name: "Search" }), { target: { value: "a query" } });
    expect(readProjectWork).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "Apply filters" }));
    await act(async () => {});
    expect(screen.queryByRole("button", { name: /Visible task/ })).not.toBeInTheDocument();
    fireEvent.change(await screen.findByRole("combobox", { name: "Kind (loaded rows)" }), { target: { value: "task" } });
    expect(screen.queryByRole("button", { name: /Visible task/ })).not.toBeInTheDocument();
    expect(screen.getByText(/1 of 1 Engram items loaded; 0 visible/)).toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(1);
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
  it("shows the empty Work state when no tracker is bound", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({sources:[{source:"engram",state:"absent",message:"No tracker is bound: no .engram-project declaration"}],page:null,readerId:null,observedAt:"now"});
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    expect(await screen.findByText("No tracker is bound to this project.")).toBeInTheDocument();
    expect(screen.queryByRole("button", {name:"Load more work"})).not.toBeInTheDocument();
    expect(readWorkDetail).not.toHaveBeenCalled();
  });
});
