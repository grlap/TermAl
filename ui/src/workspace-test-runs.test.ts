// Test Runs workspace contract: per-pane singleton, persistence, dock routing
// and transfer refusal. Uses the same public reducer seam as Work tab tests.
import { describe, expect, it } from "vitest";
import { addWorkspaceTabToPane, openTestRunsInWorkspaceState, openWorkInWorkspaceState, placeDraggedTab, placeExternalTab } from "./workspace";
import { createTestRunsTab } from "./workspace-tabs";
import { isPaneViewMode, isWorkspaceTab } from "./workspace-tab-validation";
import { formatTabLabel } from "./panels/pane-tab-labels";
import type { WorkspaceState } from "./workspace-types";

function workspace(): WorkspaceState {
  return {
    lastContentPaneId: "a", lastViewerPaneId: null, activePaneId: "a",
    root: { id: "split", type: "split", direction: "row", ratio: .5,
      first: { type: "pane", paneId: "a" }, second: { type: "pane", paneId: "b" } },
    panes: ["a", "b"].map(id => ({ id, tabs: [{ id: `${id}-session`, kind: "session", sessionId: id }],
      activeTabId: `${id}-session`, activeSessionId: id, viewMode: "session", lastSessionViewMode: "session", sourcePath: null })),
  };
}
describe("Test Runs workspace tab", () => {
  it("validates a persisted tab and its filters", () => {
    const tab = createTestRunsTab("s", "p", "s");
    expect(isWorkspaceTab(JSON.parse(JSON.stringify(tab)))).toBe(true);
    expect(isWorkspaceTab({ ...tab, filterSessionId: 4 })).toBe(false);
    expect(isPaneViewMode("testRuns")).toBe(true);
    expect(formatTabLabel(tab, null)).toBe("Test Runs");
  });
  it("reopens in place with a fresh session filter and retains Work independently", () => {
    const initial = openTestRunsInWorkspaceState(openWorkInWorkspaceState(workspace(), "a", "a"), "a", "a", null, "a");
    const first = initial.panes[0].tabs.find(tab => tab.kind === "testRuns")!;
    const next = openTestRunsInWorkspaceState(initial, "a", "b", null, "b");
    expect(next.panes[0].tabs.filter(tab => tab.kind === "testRuns")).toHaveLength(1);
    expect(next.panes[0].tabs.find(tab => tab.kind === "testRuns")).toMatchObject({ id: first.id, filterSessionId: "b" });
    expect(next.panes[0].viewMode).toBe("testRuns");
    expect(next.panes[0].tabs.some(tab => tab.kind === "work")).toBe(true);
  });
  it("refuses duplicate transfers without deleting the source, but permits reordering", () => {
    const initial = openTestRunsInWorkspaceState(openTestRunsInWorkspaceState(workspace(), "a", "a"), "b", "b");
    const tab = initial.panes[0].tabs.find(tab => tab.kind === "testRuns")!;
    expect(placeDraggedTab(initial, "a", tab.id, "b", "tabs")).toBe(initial);
    expect(placeExternalTab(initial, tab, "b", "tabs")).toBe(initial);
    expect(addWorkspaceTabToPane(initial, "b", tab)).toBe(initial);
    expect(placeDraggedTab(initial, "a", tab.id, "a", "tabs", 0).panes[0].tabs[0].id).toBe(tab.id);
    const moved = placeExternalTab(workspace(), tab, "b", "tabs");
    expect(moved.panes[1].tabs.some(candidate => candidate.kind === "testRuns")).toBe(true);
  });
  it("routes a dock action to the content pane and reuses the existing tab", () => {
    const initial = workspace();
    initial.panes[1] = { ...initial.panes[1], tabs: [{ id: "dock", kind: "controlPanel", originSessionId: null }], activeTabId: "dock", viewMode: "controlPanel" };
    const opened = openTestRunsInWorkspaceState(initial, "b", "a");
    const reopened = openTestRunsInWorkspaceState(opened, "b", "a");
    expect(reopened.panes[1].tabs).toEqual(initial.panes[1].tabs);
    expect(reopened.panes[0].tabs.filter(tab => tab.kind === "testRuns")).toHaveLength(1);
    expect(reopened.panes[0].activeTabId).toBe(opened.panes[0].activeTabId);
  });
});
