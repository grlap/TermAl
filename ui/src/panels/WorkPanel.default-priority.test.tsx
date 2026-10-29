// Owns the Work panel's default ordering and its Urgent strip in a large
// project: every open P0 and P1 is reachable in the initial default view even
// when Engram's source order (oldest first) puts it past the first two hundred
// rows, and even when lower-priority dependency chains fill the start of the
// Dependencies view; source order stays one choice away. Does not own paging,
// windowing or the other sort columns, which WorkPanel.paging.test.tsx and
// WorkPanel.test.tsx cover. New file; not split out of another.
import { fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { readProjectWork, readWorkDetail, type WorkItem, type WorkListResponse } from "../work-visualizer-api";
import { WorkPanel } from "./WorkPanel";

vi.mock("../work-visualizer-api", () => ({ readProjectWork: vi.fn(), readWorkDetail: vi.fn() }));

const projects = [{ id: "project", name: "Project", rootPath: "/project", remoteId: "local" }] as Project[];
const TOTAL = 260;
const PAGE = 100;
// Source positions past the first 200 rows, the old auto-load target.
const P0_INDEX = 230;
const HELD_P1_INDEX = 250;
const LINKED_ROWS = 200;

const ref = (index: number) => `item-${String(index).padStart(3, "0")}`;

function row(index: number, linked: boolean): WorkItem {
  const base: WorkItem = {
    id: ref(index), shortRef: ref(index), title: `Task ${index}`, kind: "task", lifecycle: "open", availability: "ready",
    priority: 2, labels: ["topic"], assignedTo: null, parentId: null, updatedAt: "today", blockedBy: [],
    source: "engram", prerequisites: [],
  };
  if (index === P0_INDEX) return { ...base, title: "Newest P0", kind: "bug", priority: 0 };
  if (index === HELD_P1_INDEX) {
    return { ...base, title: "Held P1", kind: "bug", priority: 1, availability: "active", assignedTo: "Termal::Holder" };
  }
  // In pairs: each even row waits for the next odd one, so the first 200 rows
  // form 100 blocked chains ahead of the unlinked rows.
  if (linked && index < LINKED_ROWS && index % 2 === 0) {
    return { ...base, availability: "blocked", prerequisites: [{ id: ref(index + 1), satisfied: false }] };
  }
  return base;
}

function page(start: number, linked: boolean): WorkListResponse {
  const end = Math.min(start + PAGE, TOTAL);
  return {
    readerId: "reader", observedAt: "now", sources: [{ source: "engram", state: "ready", message: "Open work" }],
    page: {
      items: Array.from({ length: end - start }, (_, i) => row(start + i, linked)), total: TOTAL, shownBefore: start,
      more: end < TOTAL, after: end < TOTAL ? `c${end}` : null, hint: null,
    },
  };
}

function serve(linked: boolean) {
  vi.mocked(readProjectWork).mockImplementation(async (_project, _filters, _signal, continuation) =>
    page(continuation ? Number(continuation.after.slice(1)) : 0, linked));
}

async function mount() {
  const view = render(<WorkPanel projects={projects} focusedProjectId="project" />);
  await screen.findByText(new RegExp(`${TOTAL} of ${TOTAL} Engram items loaded`));
  return view;
}

describe("Work panel default order and Urgent strip", () => {
  beforeEach(() => {
    vi.resetAllMocks(); localStorage.clear();
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
  });
  afterEach(() => { vi.restoreAllMocks(); });

  it("leads the default view with every open P0 and P1, including a held one created after row 200", async () => {
    serve(false);
    const { container } = await mount();
    const refs = () => [...container.querySelectorAll(".work-row-ref")].map(node => node.textContent);

    // The default view is the Dependencies tree, sorted by priority.
    expect(screen.getByRole("button", { name: "Dependencies", pressed: true })).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Sort (loaded rows)" })).toHaveValue("priority");
    expect(refs().slice(0, 3)).toEqual([ref(P0_INDEX), ref(HELD_P1_INDEX), ref(0)]);

    // Source order remains one choice away and puts the oldest rows first.
    fireEvent.change(screen.getByRole("combobox", { name: "Sort (loaded rows)" }), { target: { value: "" } });
    expect(refs().slice(0, 2)).toEqual([ref(0), ref(1)]);
  });

  it("keeps an unlinked P0 and held P1 reachable through the Urgent strip when chains fill the Dependencies view", async () => {
    serve(true);
    const { container } = await mount();

    // The Dependencies view lists its 100 lower-priority chains first, so the
    // urgent unlinked rows come after them in the view itself.
    const treeRefs = [...container.querySelectorAll(".work-row-ref")].map(node => node.textContent);
    expect(treeRefs.slice(0, 2)).toEqual([ref(0), ref(1)]);

    // The strip lists both, P0 first, in the initial default view.
    const strip = screen.getByRole("region", { name: "Urgent work" });
    expect(within(strip).getByRole("button", { name: "Urgent: 1 P0, 1 P1" })).toHaveAttribute("aria-expanded", "true");
    expect(within(strip).getAllByRole("button", { name: /^Urgent item-/ }).map(button => button.getAttribute("aria-label")))
      .toEqual([`Urgent ${ref(P0_INDEX)} — Newest P0`, `Urgent ${ref(HELD_P1_INDEX)} — Held P1`]);
    expect(within(strip).getByText("Termal::Holder")).toBeInTheDocument();

    // A strip row selects the item; its row and details open in the active view.
    fireEvent.click(within(strip).getByRole("button", { name: `Urgent ${ref(P0_INDEX)} — Newest P0` }));
    expect(await screen.findByRole("complementary", { name: "Work item details" })).toBeInTheDocument();
    expect(within(strip).getByRole("button", { name: `Urgent ${ref(P0_INDEX)} — Newest P0` })).toHaveAttribute("aria-current", "true");
    expect(screen.getByRole("button", { name: `${ref(P0_INDEX)} — Newest P0` })).toHaveAttribute("aria-current", "true");

    // The strip collapses and expands without losing its count.
    fireEvent.click(within(strip).getByRole("button", { name: "Urgent: 1 P0, 1 P1" }));
    expect(within(strip).queryByRole("button", { name: /^Urgent item-/ })).not.toBeInTheDocument();
    fireEvent.click(within(strip).getByRole("button", { name: "Urgent: 1 P0, 1 P1" }));
    expect(within(strip).getAllByRole("button", { name: /^Urgent item-/ })).toHaveLength(2);
  });
});

// Small fixtures for the reveal path: an urgent item hidden by a collapsed
// tree branch or label group opens from the strip all the same.
describe("Urgent strip reveal of hidden rows", () => {
  const item = (id: string, extra: Partial<WorkItem> = {}): WorkItem => ({
    id, shortRef: id, title: `Title ${id}`, kind: "task", lifecycle: "open", availability: "ready", priority: 2,
    labels: ["shared"], assignedTo: null, parentId: null, updatedAt: "today", blockedBy: [], source: "engram",
    prerequisites: [], ...extra,
  });
  const serveRows = (items: WorkItem[]) => vi.mocked(readProjectWork).mockResolvedValue({
    readerId: "reader", observedAt: "now", sources: [{ source: "engram", state: "ready", message: "Open work" }],
    page: { items, total: items.length, shownBefore: 0, more: false, after: null, hint: null },
  });
  const mountSmall = async (count: number) => {
    render(<WorkPanel projects={projects} focusedProjectId="project" />);
    await screen.findByText(new RegExp(`${count} of ${count} Engram items loaded`));
  };
  const clickUrgent = (ref: string) => fireEvent.click(within(screen.getByRole("region", { name: "Urgent work" }))
    .getByRole("button", { name: new RegExp(`^Urgent ${ref} — `) }));

  beforeEach(() => {
    vi.resetAllMocks(); localStorage.clear();
    vi.mocked(readWorkDetail).mockResolvedValue({ status: null, holder: null, heldUntil: null, notes: [],
      notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null, readCut: { projectPosition: 1, observedAt: "now", validUntilMs: null } } });
  });
  afterEach(() => { vi.restoreAllMocks(); });

  it("expands a collapsed Dependencies branch that holds the urgent item", async () => {
    // w-goal waits for the urgent w-urgent, so w-urgent is nested under it.
    serveRows([item("w-goal", { availability: "blocked", prerequisites: [{ id: "w-urgent", satisfied: false }] }),
      item("w-urgent", { priority: 0 })]);
    await mountSmall(2);
    fireEvent.click(screen.getByRole("button", { name: "Collapse w-goal" }));
    expect(screen.queryByRole("button", { name: "w-urgent — Title w-urgent" })).not.toBeInTheDocument();

    clickUrgent("w-urgent");
    expect(screen.getByRole("button", { name: "w-urgent — Title w-urgent" })).toHaveAttribute("aria-current", "true");
    expect(await screen.findByRole("complementary", { name: "Work item details" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Collapse w-goal" })).toBeInTheDocument();
  });

  it("expands a collapsed Hierarchy parent that holds the urgent item", async () => {
    serveRows([item("w-parent", { kind: "epic" }), item("w-child", { parentId: "w-parent", priority: 1 })]);
    await mountSmall(2);
    fireEvent.click(screen.getByRole("button", { name: "Hierarchy" }));
    fireEvent.click(screen.getByRole("button", { name: "Collapse w-parent" }));
    expect(screen.queryByRole("button", { name: "w-child — Title w-child" })).not.toBeInTheDocument();

    clickUrgent("w-child");
    expect(screen.getByRole("button", { name: "w-child — Title w-child" })).toHaveAttribute("aria-current", "true");
    expect(await screen.findByRole("complementary", { name: "Work item details" })).toBeInTheDocument();
  });

  it("expands the item's label group after Collapse all labels and opens its details there", async () => {
    serveRows([item("w-other"), item("w-urgent", { priority: 0, labels: ["alpha", "shared"] })]);
    await mountSmall(2);
    fireEvent.click(screen.getByRole("button", { name: "Labels" }));
    fireEvent.click(screen.getByRole("button", { name: "Collapse all labels" }));
    expect(screen.queryByRole("button", { name: "w-urgent — Title w-urgent" })).not.toBeInTheDocument();

    clickUrgent("w-urgent");
    // Its first label group ("alpha") opens; the others stay collapsed.
    const alpha = screen.getByRole("region", { name: "Label group: alpha" });
    expect(within(alpha).getByRole("button", { name: "w-urgent — Title w-urgent" })).toHaveAttribute("aria-current", "true");
    expect(await within(alpha).findByRole("complementary", { name: "Work item details" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "w-other — Title w-other" })).not.toBeInTheDocument();
  });
});
