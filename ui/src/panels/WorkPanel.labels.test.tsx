// Integration coverage for loaded-only label browsing, independent of the
// source filter/reader contract and without live tracker access.
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { readProjectWork, type WorkItem, type WorkListResponse } from "../work-visualizer-api";
import { WorkPanel } from "./WorkPanel";

vi.mock("../work-visualizer-api", () => ({ readProjectWork: vi.fn(), readWorkDetail: vi.fn(), readWorkBeadsDetail: vi.fn() }));
const projects = [{ id: "one", name: "One", rootPath: "/one", remoteId: "local" }, { id: "two", name: "Two", rootPath: "/two", remoteId: "local" }] as Project[];
function item(id: string, labels: string[], source = "engram"): WorkItem {
  return { id, shortRef: id, title: `Task ${id}`, labels, source, priority: 2, kind: "task", lifecycle: "open", availability: "ready", assignedTo: null, parentId: null, blockedBy: [], prerequisites: [], updatedAt: "2026-09-16T10:00:00Z" };
}
function response(): WorkListResponse {
  return { sources: [], readerId: "host:reader", observedAt: "now", page: { items: [item("a", ["storage", "reliability"]), item("b", ["storage"]), item("c", [])], total: 3, shownBefore: 0, more: false, after: null, hint: null },
    beads: { items: [item("d", ["reliability"], "beads")], total: 1, shownBefore: 0, more: false, after: null, hint: null } };
}
async function mount() {
  vi.mocked(readProjectWork).mockResolvedValue(response());
  render(<WorkPanel projects={projects} focusedProjectId="one" />);
  await screen.findByRole("button", { name: "a — Task a" });
}
const picker = () => screen.getByRole("group", { name: "Choose labels" });
const openPicker = () => fireEvent.click(screen.getByRole("button", { name: /Labels \(loaded rows\)/ }));
const group = (name: string) => screen.getByRole("region", { name: `Label group: ${name}` });

describe("Work label browsing", () => {
  beforeEach(() => { vi.resetAllMocks(); localStorage.clear(); });

  it("shows chips in both trees and the table; clicking filters locally", async () => {
    await mount();
    for (const view of ["Dependencies", "Hierarchy", "Table"]) {
      fireEvent.click(screen.getByRole("button", { name: view }));
      expect(screen.getAllByRole("button", { name: "Filter by label storage" })).toHaveLength(2);
    }
    fireEvent.click(screen.getAllByRole("button", { name: "Filter by label storage" })[0]!);
    expect(screen.getByText(/2 visible/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "c — Task c" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "d — Task d" })).not.toBeInTheDocument();
    expect(readProjectWork).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "Remove label storage" }));
    expect(screen.getByRole("button", { name: "c — Task c" })).toBeInTheDocument();
  });

  it("groups multi-label items without double-counting, keeps Unlabelled separate, and collapses groups", async () => {
    await mount();
    fireEvent.click(screen.getByRole("button", { name: "Labels" }));
    expect(screen.getByText(/4 unique items/)).toBeInTheDocument();
    expect(within(group("storage")).getByRole("button", { name: "a — Task a" })).toBeInTheDocument();
    expect(within(group("reliability")).getByRole("button", { name: "a — Task a" })).toBeInTheDocument();
    expect(within(group("Unlabelled")).getByRole("button", { name: "c — Task c" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Collapse all labels" }));
    expect(screen.queryByRole("button", { name: "a — Task a" })).not.toBeInTheDocument();
    fireEvent.click(within(group("storage")).getByRole("button", { expanded: false }));
    expect(screen.getAllByRole("button", { name: "a — Task a" })).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Expand all labels" }));
    expect(screen.getAllByRole("button", { name: "a — Task a" })).toHaveLength(2);
  });

  it("searches counted options, matches Any/All, and restores focus with Escape", async () => {
    await mount();
    openPicker();
    const search = screen.getByRole("textbox", { name: "Search labels" });
    expect(search).toHaveFocus();
    fireEvent.change(search, { target: { value: "STOR" } });
    expect(within(picker()).getAllByRole("checkbox")).toHaveLength(1);
    expect(screen.getByRole("checkbox", { name: "storage" })).toHaveAttribute("aria-description", "2 loaded items");
    fireEvent.click(screen.getByRole("checkbox", { name: "storage" }));
    fireEvent.change(search, { target: { value: "" } });
    fireEvent.click(screen.getByRole("checkbox", { name: "reliability" }));
    expect(screen.getByText(/3 visible/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "All labels" }));
    expect(screen.getByText(/1 visible/)).toBeInTheDocument();
    fireEvent.keyDown(search, { key: "Escape" });
    expect(screen.queryByRole("group", { name: "Choose labels" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Labels \(loaded rows\)/ })).toHaveFocus();
    expect(readProjectWork).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "Clear labels" }));
    expect(screen.getByText(/4 visible/)).toBeInTheDocument();
  });

  it("retains a zero-count selected label after refresh and resets selection on project change", async () => {
    await mount();
    fireEvent.click(screen.getAllByRole("button", { name: "Filter by label storage" })[0]!);
    const next = response();
    next.page!.items = [item("a", [])];
    next.page!.total = 1;
    vi.mocked(readProjectWork).mockResolvedValue(next);
    fireEvent.click(screen.getByRole("button", { name: "Refresh work" }));
    await waitFor(() => expect(screen.getByText(/1 of 1 Engram/)).toBeInTheDocument());
    expect(screen.getByText(/0 visible/)).toBeInTheDocument();
    openPicker();
    expect(screen.getByRole("checkbox", { name: "storage" })).toBeChecked();
    expect(screen.getByRole("checkbox", { name: "storage" })).toHaveAttribute("aria-description", "0 loaded items");
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("group", { name: "Choose labels" })).not.toBeInTheDocument();
    fireEvent.change(screen.getByRole("combobox", { name: "Work project" }), { target: { value: "two" } });
    await screen.findByRole("button", { name: "a — Task a" });
    expect(screen.queryByRole("button", { name: "Remove label storage" })).not.toBeInTheDocument();
  });

  it("updates counts and filtered groups as automatic paging adds labels without another filter read", async () => {
    const first = response();
    first.page!.more = true;
    first.page!.after = "next";
    first.page!.total = 4;
    let finish!: (value: WorkListResponse) => void;
    vi.mocked(readProjectWork).mockResolvedValueOnce(first).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    render(<WorkPanel projects={projects} focusedProjectId="one" />);
    await screen.findByRole("button", { name: "a — Task a" });
    fireEvent.click(screen.getAllByRole("button", { name: "Filter by label storage" })[0]!);
    fireEvent.click(screen.getByRole("button", { name: "Labels" }));
    await waitFor(() => expect(readProjectWork).toHaveBeenCalledTimes(2));
    const next = response();
    next.page = { items: [item("e", ["storage", "<img onerror=alert(1)>"])], total: 4, shownBefore: 3, more: false, after: null, hint: null };
    next.beads = null;
    await act(async () => finish(next));
    expect(screen.getByText(/3 unique items/)).toBeInTheDocument();
    expect(within(group("storage")).getByRole("button", { name: "e — Task e" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Label group: <img onerror=alert(1)>" })).toBeInTheDocument();
    expect(document.querySelector("img")).toBeNull();
    openPicker();
    expect(screen.getByRole("checkbox", { name: "storage" })).toHaveAttribute("aria-description", "3 loaded items");
    expect(readProjectWork).toHaveBeenCalledTimes(2);
  });
});
