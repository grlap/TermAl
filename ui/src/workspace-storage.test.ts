import { beforeEach, describe, expect, it } from "vitest";

import {
  getStoredWorkspaceLayout,
  persistWorkspaceLayout,
  WORKSPACE_LAYOUT_STORAGE_KEY,
  type StoredWorkspaceLayout,
} from "./workspace-storage";

describe("workspace storage", () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  it("returns null when storage is empty or invalid", () => {
    expect(getStoredWorkspaceLayout()).toBeNull();

    window.localStorage.setItem(WORKSPACE_LAYOUT_STORAGE_KEY, "not-json");
    expect(getStoredWorkspaceLayout()).toBeNull();

    window.localStorage.setItem(WORKSPACE_LAYOUT_STORAGE_KEY, JSON.stringify({ controlPanelSide: "up" }));
    expect(getStoredWorkspaceLayout()).toBeNull();
  });

  it("persists and restores a valid workspace layout", () => {
    const layout: StoredWorkspaceLayout = {
      controlPanelSide: "right",
      workspace: {
        root: {
          id: "split-1",
          type: "split",
          direction: "row",
          ratio: 0.5,
          first: {
            type: "pane",
            paneId: "pane-control",
          },
          second: {
            type: "pane",
            paneId: "pane-session",
          },
        },
        panes: [
          {
            id: "pane-control",
            tabs: [
              {
                id: "tab-control",
                kind: "controlPanel",
                originSessionId: null,
              },
            ],
            activeTabId: "tab-control",
            activeSessionId: null,
            viewMode: "controlPanel",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
          {
            id: "pane-session",
            tabs: [
              {
                id: "tab-session",
                kind: "session",
                sessionId: "session-1",
              },
              {
                id: "tab-source",
                kind: "source",
                path: "C:/repo/src/main.ts",
                originSessionId: "session-1",
              },
            ],
            activeTabId: "tab-source",
            activeSessionId: "session-1",
            viewMode: "source",
            lastSessionViewMode: "session",
            sourcePath: "C:/repo/src/main.ts",
          },
        ],
        activePaneId: "pane-session",
      },
    };

    persistWorkspaceLayout(layout);

    expect(window.localStorage.getItem(WORKSPACE_LAYOUT_STORAGE_KEY)).not.toBeNull();
    expect(getStoredWorkspaceLayout()).toEqual(layout);
  });

  it("rejects malformed workspace trees", () => {
    const malformed = {
      controlPanelSide: "left",
      workspace: {
        root: {
          type: "pane",
          paneId: "missing-pane",
        },
        panes: [
          {
            id: "pane-a",
            tabs: [
              {
                id: "tab-a",
                kind: "controlPanel",
                originSessionId: null,
              },
            ],
            activeTabId: "tab-a",
            activeSessionId: null,
            viewMode: "controlPanel",
            lastSessionViewMode: "session",
            sourcePath: null,
          },
        ],
        activePaneId: "pane-a",
      },
    };

    window.localStorage.setItem(WORKSPACE_LAYOUT_STORAGE_KEY, JSON.stringify(malformed));

    expect(getStoredWorkspaceLayout()).toBeNull();
  });
});
