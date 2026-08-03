import { describe, expect, it } from "vitest";

import {
  activatePane,
  openResponseBoardInWorkspaceState,
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
  it("keeps exactly one restorable board beside the source session", () => {
    const opened = openResponseBoardInWorkspaceState(
      splitWorkspace(),
      "pane-board",
      "session-1",
      "project-1",
    );
    const boardTabs = opened.panes.flatMap((pane) =>
      pane.tabs.filter((tab) => tab.kind === "responseBoard"),
    );
    expect(boardTabs).toHaveLength(1);
    expect(
      opened.panes.find((pane) => pane.id === "pane-board")?.viewMode,
    ).toBe("responseBoard");
    expect(isWorkspaceTab(boardTabs[0])).toBe(true);

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
    );
    expect(
      reopened.panes.flatMap((pane) =>
        pane.tabs.filter((tab) => tab.kind === "responseBoard"),
      ),
    ).toHaveLength(1);
    expect(
      reopened.panes.find((pane) => pane.id === "pane-board")?.tabVisitHistory,
    ).toEqual([boardTabs[0]?.id, "session-tab-2"]);
  });
});
