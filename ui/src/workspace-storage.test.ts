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
              {
                id: "tab-orchestrators",
                kind: "orchestratorList",
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
              {
                id: "tab-canvas",
                kind: "canvas",
                cards: [{ sessionId: "session-1", x: 160, y: 200 }],
                zoom: 1.35,
                originSessionId: "session-1",
              },
              {
                id: "tab-orchestrator-canvas",
                kind: "orchestratorCanvas",
                originSessionId: "session-1",
                originProjectId: "project-1",
                templateId: "template-1",
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

  it("rejects malformed canvas tabs", () => {
    const malformed = {
      controlPanelSide: "left",
      workspace: {
        root: {
          type: "pane",
          paneId: "pane-a",
        },
        panes: [
          {
            id: "pane-a",
            tabs: [
              {
                id: "tab-canvas",
                kind: "canvas",
                cards: [{ sessionId: "session-1", x: "bad", y: 200 }],
                originSessionId: null,
              },
            ],
            activeTabId: "tab-canvas",
            activeSessionId: null,
            viewMode: "canvas",
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

  it("rejects malformed canvas zoom values", () => {
    const malformed = {
      controlPanelSide: "left",
      workspace: {
        root: {
          type: "pane",
          paneId: "pane-a",
        },
        panes: [
          {
            id: "pane-a",
            tabs: [
              {
                id: "tab-canvas",
                kind: "canvas",
                cards: [{ sessionId: "session-1", x: 160, y: 200 }],
                zoom: "bad",
                originSessionId: null,
              },
            ],
            activeTabId: "tab-canvas",
            activeSessionId: null,
            viewMode: "canvas",
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

  it("rejects malformed orchestrator canvas tabs", () => {
    const malformed = {
      controlPanelSide: "left",
      workspace: {
        root: {
          type: "pane",
          paneId: "pane-a",
        },
        panes: [
          {
            id: "pane-a",
            tabs: [
              {
                id: "tab-orchestrator-canvas",
                kind: "orchestratorCanvas",
                originSessionId: null,
                templateId: ["bad"],
              },
            ],
            activeTabId: "tab-orchestrator-canvas",
            activeSessionId: null,
            viewMode: "orchestratorCanvas",
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
