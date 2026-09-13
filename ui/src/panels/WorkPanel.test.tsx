import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { ApiRequestError } from "../api-request";
import { readProjectWork, readWorkDetail, type WorkListResponse, type WorkDetailResponse } from "../work-visualizer-api";
import { WorkPanel } from "./WorkPanel";

vi.mock("../work-visualizer-api", () => ({ readProjectWork: vi.fn(), readWorkDetail: vi.fn() }));
const projects = [
  { id: "p-one", name: "One", rootPath: "/one", remoteId: "local" },
  { id: "p-two", name: "Two", rootPath: "/two", remoteId: "local" },
] as Project[];
function page(title = "Visible task", after: string | null = null): WorkListResponse {
  return { sources: [{ source: "engram", state: "ready", message: "Established host binding" }], readerSessionId: "session-reader", observedAt: "now",
    page: { items: [{ id: title, shortRef: "w-one", title, kind: "bug", lifecycle: "open", availability: "blocked", priority: 1, labels: ["decision"], assignedTo: "actor", parentId: null, updatedAt: "today", blockedBy: [] }], total: after ? 2 : 1, shownBefore: 0, more: !!after, after, hint: null } };
}
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(r => { resolve = r; }); return { promise, resolve }; }

describe("WorkPanel", () => {
  beforeEach(() => { vi.resetAllMocks(); localStorage.clear(); });

  it("uses the focused project and renders independent lifecycle, availability and assignment", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    render(<WorkPanel projects={projects} focusedProjectId="p-two" />);
    expect(screen.getByRole("combobox", { name: "Work project" })).toHaveValue("p-two");
    expect(await screen.findByRole("button", { name: "w-one — Visible task" })).toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledWith("p-two", { search: "", label: "", availability: "" }, expect.any(AbortSignal), undefined);
    expect(screen.getByRole("cell", { name: "open" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "blocked" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Assignment" })).toBeInTheDocument();
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
    expect(screen.getByRole("region", { name: "Work table scroll area" })).toBeInTheDocument();
  });

  it("preserves an explicit project selection across dock remounts", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    const first = render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("table");
    fireEvent.change(screen.getByRole("combobox", { name: "Work project" }), { target: { value: "p-two" } });
    await screen.findByRole("table");
    first.unmount();
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    expect(screen.getByRole("combobox", { name: "Work project" })).toHaveValue("p-two");
    await screen.findByRole("table");
    expect(readProjectWork).toHaveBeenLastCalledWith("p-two", expect.any(Object), expect.any(AbortSignal), undefined);
  });

  it("drops the old generation when a continuation becomes invalid", async () => {
    vi.mocked(readProjectWork).mockResolvedValueOnce(page("Old item", "opaque"))
      .mockRejectedValueOnce(new ApiRequestError("request-failed", "work_catalog_cursor_invalid", { status: 409 }));
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    fireEvent.click(await screen.findByRole("button", { name: "Load more work" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("work_catalog_cursor_invalid");
    expect(screen.queryByText(/Old item/)).not.toBeInTheDocument();
    expect(readProjectWork).toHaveBeenLastCalledWith("p-one", expect.any(Object), expect.any(AbortSignal), { after: "opaque", readerSessionId: "session-reader" });
  });

  it("shows a byte-limited page without repeatedly reading it", async () => {
    const limited = page(); limited.page = { items: [], total: 1, shownBefore: 0, more: true, after: null, hint: "First row exceeds budget; use show" };
    vi.mocked(readProjectWork).mockResolvedValue(limited);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    expect(await screen.findByText("First row exceeds budget; use show")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Load more work" })).not.toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(1);
  });

  it("appends exactly one page for repeated load clicks", async () => {
    const pending = deferred<WorkListResponse>();
    vi.mocked(readProjectWork).mockResolvedValueOnce(page("First", "opaque")).mockReturnValueOnce(pending.promise);
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    const more = await screen.findByRole("button", { name: "Load more work" });
    fireEvent.click(more); fireEvent.click(more);
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    const next = page("Second"); next.page!.total = 2; next.page!.shownBefore = 1;
    await act(async () => pending.resolve(next));
    expect(screen.getByRole("button", { name: "w-one — First" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "w-one — Second" })).toBeInTheDocument();
    expect(screen.getByText(/2 of 2 source items loaded/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Load more work" })).not.toBeInTheDocument();
  });

  it("renders disabled and unavailable sources without claiming the project is empty", async () => {
    vi.mocked(readProjectWork).mockResolvedValue({ sources: [{ source: "engram", state: "disabled", message: "Not enabled by operator" }], page: null, readerSessionId: null, observedAt: "now" });
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    expect(await screen.findByText(/Not enabled by operator/)).toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  it("applies source filters only on submit and labels local filtering as loaded-row filtering", async () => {
    vi.mocked(readProjectWork).mockResolvedValue(page());
    render(<WorkPanel projects={projects} focusedProjectId="p-one" />);
    await screen.findByRole("table");
    fireEvent.change(screen.getByRole("textbox", { name: "Search" }), { target: { value: "a query" } });
    expect(readProjectWork).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "Apply filters" }));
    await waitFor(() => expect(readProjectWork).toHaveBeenCalledTimes(2));
    fireEvent.change(await screen.findByRole("combobox", { name: "Kind (loaded rows)" }), { target: { value: "task" } });
    expect(screen.queryByRole("button", { name: /Visible task/ })).not.toBeInTheDocument();
    expect(screen.getByText(/1 of 1 source items loaded; 0 visible/)).toBeInTheDocument();
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
    expect(readWorkDetail).toHaveBeenLastCalledWith("p-one", "w-one", "session-reader", expect.any(AbortSignal), "first-cut");
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
