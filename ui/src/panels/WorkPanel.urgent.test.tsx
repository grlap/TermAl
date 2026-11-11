// Owns the standalone Urgent view's navigation, filtered rows and inline
// details. Exercises the panel's existing reader and selection, not a mock view.
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { readProjectWork, readWorkDetail, type WorkItem, type WorkListResponse } from "../work-visualizer-api";
import { WorkPanel } from "./WorkPanel";

vi.mock("../work-visualizer-api", () => ({ readProjectWork: vi.fn(), readWorkDetail: vi.fn() }));
const projects = [{ id: "project", name: "Project", rootPath: "/project", remoteId: "local" }] as Project[];
const item = (id: string, extra: Partial<WorkItem> = {}): WorkItem => ({
  id, shortRef: id, title: `Title ${id}`, kind: "task", lifecycle: "open", availability: "ready", priority: 2,
  labels: [], assignedTo: null, parentId: null, updatedAt: "today", blockedBy: [], source: "engram", prerequisites: [], ...extra,
});
function response(items: WorkItem[], more = false, shownBefore = 0, total = items.length): WorkListResponse {
  return { readerId: "reader", observedAt: "now", sources: [{ source: "engram", state: "ready", message: "Open work" }],
    page: { items, total, shownBefore, more, after: more ? "next" : null, hint: null } };
}
async function mount(items: WorkItem[]) {
  vi.mocked(readProjectWork).mockResolvedValue(response(items));
  render(<WorkPanel projects={projects} focusedProjectId="project" />);
  await screen.findByText(new RegExp(`${items.length} of ${items.length} Engram items loaded`));
}
const urgentView = () => fireEvent.click(screen.getByRole("button", { name: "Urgent" }));

describe("standalone Urgent work view", () => {
  beforeEach(() => {
    vi.resetAllMocks(); localStorage.clear();
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null,
        readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
  });

  it("places Urgent immediately before Dependencies and leaves Dependencies as the default without a strip", async () => {
    await mount([item("p0", { priority: 0 })]);
    const buttons = within(screen.getByRole("group", { name: "Work view" })).getAllByRole("button");
    const urgent = buttons.findIndex(button => button.textContent === "Urgent");
    expect(urgent).toBeGreaterThanOrEqual(0);
    expect(buttons[urgent + 1]).toHaveTextContent("Dependencies");
    expect(screen.getByRole("button", { name: "Dependencies", pressed: true })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Urgent work" })).not.toBeInTheDocument();
  });

  it("lists an unlinked P0 and held P1 once and opens each row's details", async () => {
    await mount([item("goal", { prerequisites: [{ id: "normal", satisfied: false }] }), item("normal"),
      item("held", { priority: 1, availability: "active", assignedTo: "Holder" }), item("unlinked", { priority: 0 })]);
    urgentView();
    const list = screen.getByRole("region", { name: "Urgent work" });
    const titles = within(list).getAllByRole("button", { name: /^(unlinked|held) — / });
    expect(titles.map(button => button.textContent)).toEqual(["unlinked — Title unlinked", "held — Title held"]);
    expect(screen.queryByRole("button", { name: "normal — Title normal" })).not.toBeInTheDocument();
    expect(within(list).getByText("Holder")).toBeInTheDocument();
    for (const ref of ["unlinked", "held"]) {
      fireEvent.click(within(list).getByRole("button", { name: `${ref} — Title ${ref}` }));
      expect(await within(list).findByRole("complementary", { name: "Work item details" })).toBeInTheDocument();
      expect(readWorkDetail).toHaveBeenLastCalledWith("project", ref, "reader", expect.any(AbortSignal), undefined);
      expect(screen.getAllByRole("complementary")).toHaveLength(1);
    }
    fireEvent.click(screen.getByRole("button", { name: "Dependencies" }));
    expect(screen.queryByRole("region", { name: "Urgent work" })).not.toBeInTheDocument();
  });

  it("keeps the reader and selected details when switching between Urgent and Table", async () => {
    await mount([item("p0", { priority: 0 })]);
    urgentView();
    fireEvent.click(screen.getByRole("button", { name: "p0 — Title p0" }));
    await screen.findByRole("complementary");
    const table = screen.getByRole("button", { name: "Table" });
    table.focus(); fireEvent.click(table);
    expect(screen.getByRole("complementary")).toBeInTheDocument();
    expect(table).toHaveFocus();
    urgentView();
    expect(screen.getByRole("complementary")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Close details" }));
    expect(screen.getByRole("button", { name: "p0 — Title p0" })).toHaveFocus();
    expect(readProjectWork).toHaveBeenCalledTimes(1);
  });

  it("applies local kind and lifecycle filters and shows a short empty state", async () => {
    await mount([item("urgent", { kind: "bug", priority: 0 }), item("normal"), item("closed", { priority: 0, lifecycle: "completed" })]);
    urgentView();
    expect(screen.getByRole("button", { name: "urgent — Title urgent" })).toBeInTheDocument();
    fireEvent.change(screen.getByRole("combobox", { name: "Kind (loaded rows)" }), { target: { value: "task" } });
    expect(screen.getByText("No urgent work in the loaded rows.")).toBeInTheDocument();
    fireEvent.change(screen.getByRole("combobox", { name: "Kind (loaded rows)" }), { target: { value: "" } });
    expect(screen.getByRole("button", { name: "urgent — Title urgent" })).toBeInTheDocument();
    fireEvent.change(screen.getByRole("combobox", { name: "Lifecycle (loaded rows)" }), { target: { value: "closed" } });
    expect(screen.getByText("No urgent work in the loaded rows.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "closed — Title closed" })).not.toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(1);
  });

  it("adds later-page urgent rows without replacing selected details or starting a new reader", async () => {
    let resolve!: (value: WorkListResponse) => void;
    const next = new Promise<WorkListResponse>(done => { resolve = done; });
    vi.mocked(readProjectWork).mockResolvedValueOnce(response([item("first", { priority: 0 })], true, 0, 2)).mockReturnValueOnce(next);
    render(<WorkPanel projects={projects} focusedProjectId="project" />);
    await screen.findByText(/1 of 2 Engram items loaded/);
    urgentView();
    fireEvent.click(screen.getByRole("button", { name: "first — Title first" }));
    const details = await screen.findByRole("complementary");
    await act(async () => resolve(response([item("later", { priority: 1, availability: "active" })], false, 1, 2)));
    expect(screen.getByRole("button", { name: "later — Title later" })).toBeInTheDocument();
    expect(screen.getByRole("complementary")).toBe(details);
    expect(readProjectWork).toHaveBeenCalledTimes(2);
    expect(readProjectWork).toHaveBeenLastCalledWith("project", expect.anything(), expect.any(AbortSignal), { readerId: "reader", after: "next" });
  });
});
