import { describe, expect, it } from "vitest";

import {
  activatePane,
  openResponseBoardInWorkspaceState,
  setResponseBoardWorkspaceState,
  type WorkspaceState,
} from "./workspace";
import { isWorkspaceTab } from "./workspace-tab-validation";

function splitWorkspace(): WorkspaceState {
  return {
    root: {
      id: "split-1",
      type: "split",
      direction: "row",
      ratio: 0.5,
      first: { type: "pane", paneId: "pane-session" },
      second: { type: "pane", paneId: "pane-board" },
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
        id: "pane-board",
        tabs: [
          { id: "session-tab-2", kind: "session", sessionId: "session-2" },
        ],
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

describe("response-board workspace tab", () => {
  it("keeps one restorable board per pane with independent inner-tab state", () => {
    const opened = openResponseBoardInWorkspaceState(
      splitWorkspace(),
      "pane-board",
      "session-1",
      "project-1",
      "project-board-1",
    );
    const boardTabs = opened.panes.flatMap((pane) =>
      pane.tabs.filter((tab) => tab.kind === "responseBoard"),
    );
    expect(boardTabs).toHaveLength(1);
    expect(
      opened.panes.find((pane) => pane.id === "pane-board")?.viewMode,
    ).toBe("responseBoard");
    expect(isWorkspaceTab(boardTabs[0])).toBe(true);
    expect(boardTabs[0]?.activeBoardTabId).toBe("project-board-1");

    const returnedToSession = activatePane(
      opened,
      "pane-board",
      "session-tab-2",
    );
    const reopened = openResponseBoardInWorkspaceState(
      returnedToSession,
      "pane-session",
      "session-2",
      "project-2",
      "project-board-2",
    );
    expect(
      reopened.panes.flatMap((pane) =>
        pane.tabs.filter((tab) => tab.kind === "responseBoard"),
      ),
    ).toHaveLength(2);
    expect(
      reopened.panes.find((pane) => pane.id === "pane-board")?.tabVisitHistory,
    ).toEqual(["session-tab-2", boardTabs[0]?.id]);

    const secondBoard = reopened.panes
      .find((pane) => pane.id === "pane-session")
      ?.tabs.find((tab) => tab.kind === "responseBoard");
    expect(secondBoard?.activeBoardTabId).toBe("project-board-2");
    const firstBoardAfterSecondOpen = reopened.panes
      .find((pane) => pane.id === "pane-board")
      ?.tabs.find((tab) => tab.kind === "responseBoard");
    expect(firstBoardAfterSecondOpen?.activeBoardTabId).toBe(
      "project-board-1",
    );

    const persistedView = setResponseBoardWorkspaceState(
      reopened,
      secondBoard?.id ?? "",
      "custom-board",
      { panX: 18, panY: -9, zoom: 1.25 },
    );
    const persistedBoard = persistedView.panes
      .find((pane) => pane.id === "pane-session")
      ?.tabs.find((tab) => tab.kind === "responseBoard");
    expect(persistedBoard).toMatchObject({
      activeBoardTabId: "custom-board",
      boardViews: {
        "custom-board": { panX: 18, panY: -9, zoom: 1.25 },
      },
    });
  });

  it("prunes camera state for board tabs that no longer exist", () => {
    const opened = openResponseBoardInWorkspaceState(
      splitWorkspace(),
      "pane-board",
      "session-1",
      "project-1",
      "kept-board",
    );
    const workspaceTabId = opened.panes
      .find((pane) => pane.id === "pane-board")
      ?.tabs.find((tab) => tab.kind === "responseBoard")?.id;
    const withTwoViews = setResponseBoardWorkspaceState(
      setResponseBoardWorkspaceState(
        opened,
        workspaceTabId ?? "",
        "kept-board",
        { panX: 10, panY: 20, zoom: 1.1 },
      ),
      workspaceTabId ?? "",
      "removed-board",
      { panX: 30, panY: 40, zoom: 0.8 },
    );

    const pruned = setResponseBoardWorkspaceState(
      withTwoViews,
      workspaceTabId ?? "",
      "kept-board",
      { panX: 10, panY: 20, zoom: 1.1 },
      ["kept-board"],
    );
    const boardTab = pruned.panes
      .find((pane) => pane.id === "pane-board")
      ?.tabs.find((tab) => tab.kind === "responseBoard");
    expect(boardTab?.boardViews).toEqual({
      "kept-board": { panX: 10, panY: 20, zoom: 1.1 },
    });
  });
});
