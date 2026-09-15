// Owns the Work workspace tab's reducer contract: one restorable tab per
// pane, origin refresh on reopen, pane view mode, persisted-shape validation
// and the pane/drag labels. Mirrors workspace-response-board.test.ts.
import { describe, expect, it } from "vitest";

import { formatTabLabel } from "./panels/pane-tab-labels";
import { activatePane, addWorkspaceTabToPane, openWorkInWorkspaceState, placeDraggedTab, placeExternalTab } from "./workspace";
import { isWorkspaceTab } from "./workspace-tab-validation";
import { createWorkTab } from "./workspace-tabs";
import { type WorkspaceState } from "./workspace-types";

function splitWorkspace(): WorkspaceState {
  return {
    lastContentPaneId: null,
    lastViewerPaneId: null,
    root: {
      id: "split-1",
      type: "split",
      direction: "row",
      ratio: 0.5,
      first: { type: "pane", paneId: "pane-session" },
      second: { type: "pane", paneId: "pane-work" },
    },
    panes: [
      {
        id: "pane-session",
        tabs: [{ id: "session-tab", kind: "session", sessionId: "session-1" }],
        activeTabId: "session-tab",
        activeSessionId: "session-1",
        viewMode: "session",
        lastSessionViewMode: "session",
        sourcePath: null,
      },
      {
        id: "pane-work",
        tabs: [{ id: "session-tab-2", kind: "session", sessionId: "session-2" }],
        activeTabId: "session-tab-2",
        activeSessionId: "session-2",
        viewMode: "session",
        lastSessionViewMode: "session",
        sourcePath: null,
      },
    ],
    activePaneId: "pane-session",
  };
}

function workTabs(workspace: WorkspaceState) {
  return workspace.panes.flatMap((pane) =>
    pane.tabs.filter((tab) => tab.kind === "work"),
  );
}

describe("work workspace tab", () => {
  it.each([false, true])("refuses duplicate Work transfers without changing either pane (sole source tab: %s)", (soleSourceTab) => {
    const opened = openWorkInWorkspaceState(splitWorkspace(), "pane-work", "session-2");
    const workspace = openWorkInWorkspaceState(opened, "pane-session", "session-1");
    const source = workspace.panes.find((pane) => pane.id === "pane-session")!;
    const destination = workspace.panes.find((pane) => pane.id === "pane-work")!;
    const dragged = source.tabs.find((tab) => tab.kind === "work")!;
    const existing = destination.tabs.find((tab) => tab.kind === "work")!;
    if (soleSourceTab) source.tabs = [dragged];

    expect(placeDraggedTab(workspace, source.id, dragged.id, destination.id, "tabs")).toBe(workspace);
    // External transfer code uses unchanged object identity to refuse source removal.
    expect(placeExternalTab(workspace, dragged, destination.id, "tabs")).toBe(workspace);
    expect(addWorkspaceTabToPane(workspace, destination.id, dragged)).toBe(workspace);
    expect(workTabs(workspace)).toHaveLength(2);
    const reopened = openWorkInWorkspaceState(workspace, destination.id, "session-2");
    expect(reopened.panes.find((pane) => pane.id === destination.id)?.activeTabId).toBe(existing.id);
    expect(reopened.panes.find((pane) => pane.id === source.id)?.tabs).toEqual(source.tabs);
  });

  it("allows Work reordering and transfers to a pane without Work", () => {
    const workspace = openWorkInWorkspaceState(splitWorkspace(), "pane-work", "session-2");
    const [tab] = workTabs(workspace);
    const reordered = placeDraggedTab(workspace, "pane-work", tab!.id, "pane-work", "tabs", 0);
    expect(reordered.panes.find((pane) => pane.id === "pane-work")?.tabs[0]?.id).toBe(tab!.id);
    expect(workTabs(reordered)).toHaveLength(1);
    const moved = placeDraggedTab(workspace, "pane-work", tab!.id, "pane-session", "tabs");
    expect(workTabs(moved)).toHaveLength(1);
    expect(moved.panes.find((pane) => pane.id === "pane-session")?.activeTabId).toBe(tab!.id);
    expect(moved.panes.find((pane) => pane.id === "pane-work")?.tabs.some((entry) => entry.kind === "work")).toBe(false);
    const external = placeExternalTab(splitWorkspace(), tab!, "pane-session", "tabs", undefined, "transferred-work");
    expect(workTabs(external)).toHaveLength(1);
    expect(external.panes.find((pane) => pane.id === "pane-session")?.activeTabId).toBe("transferred-work");
  });

  it("keeps one restorable Work tab per pane and refreshes it on reopen", () => {
    const opened = openWorkInWorkspaceState(
      splitWorkspace(),
      "pane-work",
      "session-1",
      "project-1",
    );
    const [firstTab] = workTabs(opened);
    expect(workTabs(opened)).toHaveLength(1);
    expect(firstTab).toMatchObject({
      kind: "work",
      originSessionId: "session-1",
      originProjectId: "project-1",
    });
    expect(isWorkspaceTab(firstTab)).toBe(true);
    const workPane = opened.panes.find((pane) => pane.id === "pane-work");
    expect(workPane?.viewMode).toBe("work");
    expect(workPane?.activeTabId).toBe(firstTab?.id);
    expect(workPane?.activeSessionId).toBe("session-1");
    expect(opened.activePaneId).toBe("pane-work");

    const returnedToSession = activatePane(opened, "pane-work", "session-tab-2");
    const reopenedSamePane = openWorkInWorkspaceState(
      returnedToSession,
      "pane-work",
      "session-2",
      "project-2",
    );
    const [reopenedTab] = workTabs(reopenedSamePane);
    expect(workTabs(reopenedSamePane)).toHaveLength(1);
    expect(reopenedTab?.id).toBe(firstTab?.id);
    expect(reopenedTab?.refreshToken).not.toBe(firstTab?.refreshToken);
    expect(reopenedTab).toMatchObject({
      originSessionId: "session-2",
      originProjectId: "project-2",
    });
    expect(
      reopenedSamePane.panes.find((pane) => pane.id === "pane-work")?.activeTabId,
    ).toBe(firstTab?.id);

    const withoutPreferredPane = openWorkInWorkspaceState(
      activatePane(reopenedSamePane, "pane-work", "session-tab-2"),
      null,
      "session-1",
      null,
    );
    expect(workTabs(withoutPreferredPane)).toHaveLength(1);
    expect(withoutPreferredPane.activePaneId).toBe("pane-work");
    const [clearedTab] = workTabs(withoutPreferredPane);
    expect(clearedTab?.originSessionId).toBe("session-1");
    expect(clearedTab && "originProjectId" in clearedTab).toBe(false);

    const secondPane = openWorkInWorkspaceState(
      reopenedSamePane,
      "pane-session",
      "session-1",
      "project-1",
    );
    expect(workTabs(secondPane)).toHaveLength(2);
    expect(
      secondPane.panes.find((pane) => pane.id === "pane-session")?.viewMode,
    ).toBe("work");
  });

  it("reuses the content pane's Work tab when the dock launcher opens it repeatedly", () => {
    const withDock: WorkspaceState = {
      lastContentPaneId: null,
      lastViewerPaneId: null,
      root: {
        id: "split-1",
        type: "split",
        direction: "row",
        ratio: 0.3,
        first: { type: "pane", paneId: "pane-dock" },
        second: { type: "pane", paneId: "pane-session" },
      },
      panes: [
        {
          id: "pane-dock",
          tabs: [{ id: "dock-tab", kind: "controlPanel", originSessionId: null }],
          activeTabId: "dock-tab",
          activeSessionId: null,
          viewMode: "controlPanel",
          lastSessionViewMode: "session",
          sourcePath: null,
        },
        {
          id: "pane-session",
          tabs: [{ id: "session-tab", kind: "session", sessionId: "session-1" }],
          activeTabId: "session-tab",
          activeSessionId: "session-1",
          viewMode: "session",
          lastSessionViewMode: "session",
          sourcePath: null,
        },
      ],
      activePaneId: "pane-session",
    };
    let workspace = withDock;
    const tokens: string[] = [];
    for (let click = 0; click < 3; click += 1) {
      workspace = openWorkInWorkspaceState(workspace, "pane-dock", "session-1", "project-1");
      const tabs = workTabs(workspace);
      expect(tabs).toHaveLength(1);
      tokens.push(tabs[0]!.refreshToken);
    }
    expect(new Set(tokens).size).toBe(3);
    expect(
      workspace.panes.find((pane) => pane.id === "pane-dock")?.tabs.some((tab) => tab.kind === "work"),
    ).toBe(false);
    expect(workspace.panes.find((pane) => pane.id === "pane-session")?.viewMode).toBe("work");
  });

  it("validates the persisted shape and labels the tab", () => {
    const tab = createWorkTab(" session-1 ", "");
    expect(tab).toMatchObject({ kind: "work", originSessionId: "session-1" });
    expect("originProjectId" in tab).toBe(false);
    expect(isWorkspaceTab(tab)).toBe(true);
    expect(
      isWorkspaceTab({ id: "work-1", kind: "work", originSessionId: null }),
    ).toBe(false);
    expect(
      isWorkspaceTab({
        id: "work-1",
        kind: "work",
        originSessionId: null,
        refreshToken: "token",
        originProjectId: 7,
      }),
    ).toBe(false);
    expect(formatTabLabel(tab, null)).toBe("Work");
  });
});
