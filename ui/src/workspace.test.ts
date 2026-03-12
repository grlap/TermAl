import type { Session } from "./types";
import {
  addWorkspaceTabToPane,
  closeWorkspaceTab,
  createPane,
  getSplitRatio,
  openFilesystemInWorkspaceState,
  openGitStatusInWorkspaceState,
  openSessionInWorkspaceState,
  openSourceInWorkspaceState,
  placeDraggedTab,
  reconcileWorkspaceState,
  setPaneSourcePath,
  splitPane,
  updateSplitRatio,
  type WorkspacePane,
  type WorkspaceState,
  type WorkspaceTab,
} from "./workspace";

function makeSession(id: string): Session {
  return {
    id,
    name: id,
    emoji: "x",
    agent: "Codex",
    workdir: "/tmp",
    model: "test-model",
    status: "idle",
    preview: "",
    messages: [],
  };
}

function makeSessionTab(id: string, sessionId: string): WorkspaceTab {
  return {
    id,
    kind: "session",
    sessionId,
  };
}

function makeSourceTab(id: string, path: string | null, originSessionId: string | null): WorkspaceTab {
  return {
    id,
    kind: "source",
    path,
    originSessionId,
  };
}

function makeFilesystemTab(
  id: string,
  rootPath: string | null,
  originSessionId: string | null,
): WorkspaceTab {
  return {
    id,
    kind: "filesystem",
    rootPath,
    originSessionId,
  };
}

function makeGitStatusTab(
  id: string,
  workdir: string | null,
  originSessionId: string | null,
): WorkspaceTab {
  return {
    id,
    kind: "gitStatus",
    workdir,
    originSessionId,
  };
}

function makePane(
  id: string,
  tabs: WorkspaceTab[],
  options?: {
    activeTabId?: string | null;
    activeSessionId?: string | null;
    viewMode?: WorkspacePane["viewMode"];
    lastSessionViewMode?: WorkspacePane["lastSessionViewMode"];
    sourcePath?: string | null;
  },
): WorkspacePane {
  return {
    id,
    tabs,
    activeTabId: options?.activeTabId ?? tabs[0]?.id ?? null,
    activeSessionId: options?.activeSessionId ?? firstSessionId(tabs),
    viewMode: options?.viewMode ?? "session",
    lastSessionViewMode: options?.lastSessionViewMode ?? "session",
    sourcePath: options?.sourcePath ?? null,
  };
}

function makeSinglePaneWorkspace(pane: WorkspacePane): WorkspaceState {
  return {
    root: {
      type: "pane",
      paneId: pane.id,
    },
    panes: [pane],
    activePaneId: pane.id,
  };
}

function makeSplitWorkspace(
  firstPane: WorkspacePane,
  secondPane: WorkspacePane,
  activePaneId: string = firstPane.id,
): WorkspaceState {
  return {
    root: {
      id: "split-1",
      type: "split",
      direction: "row",
      ratio: 0.5,
      first: {
        type: "pane",
        paneId: firstPane.id,
      },
      second: {
        type: "pane",
        paneId: secondPane.id,
      },
    },
    panes: [firstPane, secondPane],
    activePaneId,
  };
}

function firstSessionId(tabs: WorkspaceTab[]) {
  for (const tab of tabs) {
    if (tab.kind === "session") {
      return tab.sessionId;
    }
  }

  return null;
}

describe("workspace helpers", () => {
  it("createPane returns an empty pane by default", () => {
    const pane = createPane();

    expect(pane.tabs).toEqual([]);
    expect(pane.activeTabId).toBeNull();
    expect(pane.activeSessionId).toBeNull();
    expect(pane.viewMode).toBe("session");
    expect(pane.sourcePath).toBeNull();
    expect(typeof pane.id).toBe("string");
    expect(pane.id.length).toBeGreaterThan(0);
  });

  it("openSessionInWorkspaceState creates the first pane for an empty workspace", () => {
    const next = openSessionInWorkspaceState(
      {
        root: null,
        panes: [],
        activePaneId: null,
      },
      "session-a",
      null,
    );

    expect(next.panes).toHaveLength(1);
    expect(next.panes[0].tabs).toEqual([
      expect.objectContaining({
        kind: "session",
        sessionId: "session-a",
      }),
    ]);
    expect(next.panes[0].activeSessionId).toBe("session-a");
    expect(next.activePaneId).toBe(next.panes[0].id);
    expect(next.root).toEqual({
      type: "pane",
      paneId: next.panes[0].id,
    });
  });

  it("openSessionInWorkspaceState focuses the existing session tab instead of duplicating it", () => {
    const paneA = makePane("pane-a", [makeSessionTab("tab-a", "session-a")]);
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openSessionInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "session-a",
      paneB.id,
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")?.activeTabId).toBe("tab-a");
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([makeSessionTab("tab-b", "session-b")]);
  });

  it("addWorkspaceTabToPane appends and activates without duplicating by tab id", () => {
    const initial = makeSinglePaneWorkspace(makePane("pane-a", [makeSessionTab("tab-a", "session-a")]));
    const next = addWorkspaceTabToPane(initial, "pane-a", makeSessionTab("tab-b", "session-b"));
    const deduped = addWorkspaceTabToPane(next, "pane-a", next.panes[0].tabs[1]);

    expect(next.panes[0].tabs.map((tab) => tab.id)).toEqual(["tab-a", "tab-b"]);
    expect(next.panes[0].activeSessionId).toBe("session-b");
    expect(deduped.panes[0].tabs.map((tab) => tab.id)).toEqual(["tab-a", "tab-b"]);
  });

  it("closeWorkspaceTab removes a tab and selects the next one", () => {
    const next = closeWorkspaceTab(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a"), makeSessionTab("tab-b", "session-b")], {
          activeTabId: "tab-a",
          activeSessionId: "session-a",
        }),
      ),
      "pane-a",
      "tab-a",
    );

    expect(next.panes[0].tabs.map((tab) => tab.id)).toEqual(["tab-b"]);
    expect(next.panes[0].activeTabId).toBe("tab-b");
    expect(next.panes[0].activeSessionId).toBe("session-b");
    expect(next.activePaneId).toBe("pane-a");
  });

  it("closeWorkspaceTab removes the pane when its last tab closes", () => {
    const next = closeWorkspaceTab(
      makeSinglePaneWorkspace(makePane("pane-a", [makeSessionTab("tab-a", "session-a")])),
      "pane-a",
      "tab-a",
    );

    expect(next.root).toBeNull();
    expect(next.panes).toEqual([]);
    expect(next.activePaneId).toBeNull();
  });

  it("openSourceInWorkspaceState creates a source tab and switches the pane to source mode", () => {
    const next = openSourceInWorkspaceState(
      makeSinglePaneWorkspace(makePane("pane-a", [makeSessionTab("tab-a", "session-a")])),
      "/tmp/app.ts",
      "pane-a",
      "session-a",
    );

    expect(next.panes[0].tabs).toHaveLength(2);
    expect(next.panes[0].tabs[1]).toEqual({
      id: expect.any(String),
      kind: "source",
      path: "/tmp/app.ts",
      originSessionId: "session-a",
    });
    expect(next.panes[0].viewMode).toBe("source");
    expect(next.panes[0].sourcePath).toBe("/tmp/app.ts");
    expect(next.panes[0].activeSessionId).toBe("session-a");
  });

  it("openSourceInWorkspaceState focuses an existing source tab for the same path", () => {
    const paneA = makePane("pane-a", [makeSourceTab("source-a", "/tmp/app.ts", "session-a")], {
      activeTabId: "source-a",
      activeSessionId: "session-a",
      viewMode: "source",
      sourcePath: "/tmp/app.ts",
    });
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openSourceInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "/tmp/app.ts",
      paneB.id,
      "session-b",
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")?.activeTabId).toBe("source-a");
  });

  it("openFilesystemInWorkspaceState creates a filesystem tab and switches the pane mode", () => {
    const next = openFilesystemInWorkspaceState(
      makeSinglePaneWorkspace(makePane("pane-a", [makeSessionTab("tab-a", "session-a")])),
      "/tmp/project",
      "pane-a",
      "session-a",
    );

    expect(next.panes[0].tabs).toHaveLength(2);
    expect(next.panes[0].tabs[1]).toEqual({
      id: expect.any(String),
      kind: "filesystem",
      rootPath: "/tmp/project",
      originSessionId: "session-a",
    });
    expect(next.panes[0].viewMode).toBe("filesystem");
    expect(next.panes[0].activeSessionId).toBe("session-a");
  });

  it("openFilesystemInWorkspaceState focuses an existing filesystem tab for the same root", () => {
    const paneA = makePane("pane-a", [makeFilesystemTab("fs-a", "/tmp/project", "session-a")], {
      activeTabId: "fs-a",
      activeSessionId: "session-a",
      viewMode: "filesystem",
    });
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openFilesystemInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "/tmp/project",
      paneB.id,
      "session-b",
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")?.activeTabId).toBe("fs-a");
  });

  it("openGitStatusInWorkspaceState creates a git status tab and switches the pane mode", () => {
    const next = openGitStatusInWorkspaceState(
      makeSinglePaneWorkspace(makePane("pane-a", [makeSessionTab("tab-a", "session-a")])),
      "/tmp/project",
      "pane-a",
      "session-a",
    );

    expect(next.panes[0].tabs).toHaveLength(2);
    expect(next.panes[0].tabs[1]).toEqual({
      id: expect.any(String),
      kind: "gitStatus",
      workdir: "/tmp/project",
      originSessionId: "session-a",
    });
    expect(next.panes[0].viewMode).toBe("gitStatus");
    expect(next.panes[0].activeSessionId).toBe("session-a");
  });

  it("openGitStatusInWorkspaceState focuses an existing git status tab for the same workdir", () => {
    const paneA = makePane("pane-a", [makeGitStatusTab("git-a", "/tmp/project", "session-a")], {
      activeTabId: "git-a",
      activeSessionId: "session-a",
      viewMode: "gitStatus",
    });
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openGitStatusInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "/tmp/project",
      paneB.id,
      "session-b",
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")?.activeTabId).toBe("git-a");
  });

  it("setPaneSourcePath updates the active source tab path", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane("pane-a", [makeSourceTab("source-a", null, "session-a")], {
        activeTabId: "source-a",
        activeSessionId: "session-a",
        viewMode: "source",
      }),
    );

    const next = setPaneSourcePath(workspace, "pane-a", "/tmp/next.ts");
    const sourceTab = next.panes[0].tabs[0];

    expect(sourceTab).toEqual({
      id: "source-a",
      kind: "source",
      path: "/tmp/next.ts",
      originSessionId: "session-a",
    });
    expect(next.panes[0].sourcePath).toBe("/tmp/next.ts");
  });

  it("setPaneSourcePath focuses an existing source tab for the same path instead of duplicating it", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane(
        "pane-a",
        [makeSourceTab("source-a", "/tmp/app.ts", "session-a"), makeSourceTab("source-b", null, "session-a")],
        {
          activeTabId: "source-b",
          activeSessionId: "session-a",
          viewMode: "source",
        },
      ),
    );

    const next = setPaneSourcePath(workspace, "pane-a", "/tmp/app.ts");

    expect(next.panes[0].activeTabId).toBe("source-a");
    expect(next.panes[0].sourcePath).toBe("/tmp/app.ts");
  });

  it("splitPane creates an adjacent pane and moves the active tab into it", () => {
    const next = splitPane(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a"), makeSessionTab("tab-b", "session-b")], {
          activeTabId: "tab-b",
          activeSessionId: "session-b",
        }),
      ),
      "pane-a",
      "row",
    );

    expect(next.panes).toHaveLength(2);
    expect(next.activePaneId).not.toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      tabs: [makeSessionTab("tab-a", "session-a")],
      activeSessionId: "session-a",
    });
    expect(next.panes.find((pane) => pane.id !== "pane-a")).toMatchObject({
      tabs: [makeSessionTab("tab-b", "session-b")],
      activeSessionId: "session-b",
    });
    expect(next.root).toMatchObject({
      type: "split",
      direction: "row",
      first: {
        type: "pane",
        paneId: "pane-a",
      },
    });
  });

  it("placeDraggedTab moves a tab between panes without creating duplicates", () => {
    const next = placeDraggedTab(
      makeSplitWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
        makePane("pane-b", [makeSessionTab("tab-b", "session-b"), makeSessionTab("tab-c", "session-c")]),
      ),
      "pane-a",
      "tab-a",
      "pane-b",
      "tabs",
      1,
    );

    expect(next.activePaneId).toBe("pane-b");
    expect(next.panes[0]).toMatchObject({
      id: "pane-b",
      tabs: [makeSessionTab("tab-b", "session-b"), makeSessionTab("tab-a", "session-a"), makeSessionTab("tab-c", "session-c")],
      activeSessionId: "session-a",
    });
  });

  it("updateSplitRatio changes the selected split ratio and getSplitRatio reads it back", () => {
    const workspace = makeSplitWorkspace(
      makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      makePane("pane-b", [makeSessionTab("tab-b", "session-b")]),
    );

    const next = updateSplitRatio(workspace, "split-1", 0.75);

    expect(getSplitRatio(next.root, "split-1")).toBe(0.75);
  });

  it("reconcileWorkspaceState prunes missing session tabs, keeps source tabs, and recreates an initial pane when needed", () => {
    const pruned = reconcileWorkspaceState(
      makeSinglePaneWorkspace(
        makePane(
          "pane-a",
          [makeSessionTab("tab-a", "session-a"), makeSessionTab("tab-b", "session-b"), makeSourceTab("source-a", "/tmp/a.ts", "session-b")],
          {
            activeTabId: "source-a",
            activeSessionId: "session-b",
            viewMode: "source",
            sourcePath: "/tmp/a.ts",
          },
        ),
      ),
      [makeSession("session-a")],
    );

    expect(pruned.panes[0].tabs).toEqual([
      makeSessionTab("tab-a", "session-a"),
      makeSourceTab("source-a", "/tmp/a.ts", null),
    ]);
    expect(pruned.panes[0].activeTabId).toBe("source-a");
    expect(pruned.panes[0].activeSessionId).toBe("session-a");

    const rebuilt = reconcileWorkspaceState(
      {
        root: null,
        panes: [],
        activePaneId: null,
      },
      [makeSession("session-c")],
    );

    expect(rebuilt.panes).toHaveLength(1);
    expect(rebuilt.panes[0].tabs).toEqual([
      expect.objectContaining({
        kind: "session",
        sessionId: "session-c",
      }),
    ]);
    expect(rebuilt.root).toEqual({
      type: "pane",
      paneId: rebuilt.panes[0].id,
    });
  });
});
