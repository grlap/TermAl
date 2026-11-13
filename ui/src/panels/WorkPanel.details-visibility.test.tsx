import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Project } from "../types";
import { readProjectWork, readWorkDetail, type WorkDetailResponse } from "../work-visualizer-api";
import { WorkPanel } from "./WorkPanel";

vi.mock("../work-visualizer-api", () => ({ readProjectWork: vi.fn(), readWorkDetail: vi.fn() }));
const projects = [{ id: "project", name: "Project", rootPath: "/fixture", remoteId: "local" }] as Project[];
const outcome = "The final part of these expanded details must remain reachable.";
const detail = (): WorkDetailResponse => ({
  status: { availability: "ready", work: { shortRef: "example", title: "Example item", outcome,
    acceptance: ["A visible outcome"], acceptanceOmitted: null, lifecycle: "open", kind: "bug", priority: 1 } },
  holder: null, heldUntil: null, notes: [],
  notesWindow: { total: 0, shown: 0, newer: 0, older: 0, after: null,
    readCut: { projectPosition: 1, observedAt: "2026-11-13T00:00:00Z", validUntilMs: null } },
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}

describe("expanded Work details visibility", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    localStorage.clear();
    vi.mocked(readProjectWork).mockResolvedValue({
      readerId: "fixture-reader", observedAt: "2026-11-13T00:00:00Z",
      sources: [{ source: "engram", state: "ready", message: "Fixture" }],
      page: { total: 1, shownBefore: 0, more: false, after: null, hint: null, items: [{
        id: "example", shortRef: "example", title: "Example item", kind: "bug", lifecycle: "open",
        availability: "ready", priority: 1, labels: ["fixture"], assignedTo: null, parentId: null,
        updatedAt: "2026-11-13T00:00:00Z", blockedBy: [], source: "engram", prerequisites: [],
      }] },
    });
  });
  afterEach(() => vi.restoreAllMocks());

  it.each(["Dependencies", "Labels", "Table", "Urgent"])("reveals the loaded details body in %s", async view => {
    const response = deferred<WorkDetailResponse>();
    vi.mocked(readWorkDetail).mockReturnValue(response.promise);
    const revealedBodies: HTMLElement[] = [];
    // jsdom has no layout. Observe the browser scroll request when the body
    // exists, rather than counting the earlier request for the loading header.
    vi.spyOn(HTMLElement.prototype, "scrollIntoView").mockImplementation(function (this: HTMLElement) {
      if (this.textContent?.includes(outcome)) revealedBodies.push(this);
    });
    render(<WorkPanel projects={projects} focusedProjectId="project" />);
    await screen.findByRole("button", { name: "example — Example item" });
    fireEvent.click(screen.getByRole("button", { name: view }));
    fireEvent.click(screen.getByRole("button", { name: "example — Example item" }));
    const panel = screen.getByRole("complementary", { name: "Work item details" });
    expect(panel).toHaveFocus();
    expect(screen.getByText("Reading Engram details…")).toBeInTheDocument();
    expect(revealedBodies).toEqual([]);

    await act(async () => response.resolve(detail()));

    expect(screen.getByText(outcome)).toBeInTheDocument();
    expect(revealedBodies).toContain(panel);
    expect(panel).toHaveFocus();
  });

  it.each(["Dependencies", "Labels", "Table", "Urgent"])("keeps the user's new focus and view in %s", async view => {
    const response = deferred<WorkDetailResponse>();
    vi.mocked(readWorkDetail).mockReturnValue(response.promise);
    const scroll = vi.spyOn(HTMLElement.prototype, "scrollIntoView");
    render(<WorkPanel projects={projects} focusedProjectId="project" />);
    await screen.findByRole("button", { name: "example — Example item" });
    fireEvent.click(screen.getByRole("button", { name: view }));
    fireEvent.click(screen.getByRole("button", { name: "example — Example item" }));
    const search = screen.getByRole("textbox", { name: "Search" });
    search.focus();
    scroll.mockClear();

    await act(async () => response.resolve(detail()));

    expect(screen.getByText(outcome)).toBeInTheDocument();
    expect(search).toHaveFocus();
    expect(scroll).not.toHaveBeenCalled();
  });
});
