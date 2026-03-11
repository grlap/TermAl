import type { Session } from "./types";
import {
  addSessionToPane,
  closeSessionTab,
  createPane,
  getSplitRatio,
  openSessionInWorkspaceState,
  placeDraggedSession,
  reconcileWorkspaceState,
  splitPane,
  updateSplitRatio,
  type WorkspacePane,
  type WorkspaceState,
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

function makePane(
  id: string,
  sessionIds: string[],
  activeSessionId: string | null = sessionIds[0] ?? null,
): WorkspacePane {
  return {
    id,
    sessionIds,
    activeSessionId,
    viewMode: "session",
    sourcePath: null,
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

describe("workspace helpers", () => {
  it("createPane returns an empty pane by default", () => {
    const pane = createPane();

    expect(pane.sessionIds).toEqual([]);
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
    expect(next.panes[0].sessionIds).toEqual(["session-a"]);
    expect(next.panes[0].activeSessionId).toBe("session-a");
    expect(next.activePaneId).toBe(next.panes[0].id);
    expect(next.root).toEqual({
      type: "pane",
      paneId: next.panes[0].id,
    });
  });

  it("openSessionInWorkspaceState focuses the existing pane instead of duplicating the session", () => {
    const paneA = makePane("pane-a", ["session-a"]);
    const paneB = makePane("pane-b", ["session-b"]);

    const next = openSessionInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "session-a",
      paneB.id,
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")?.sessionIds).toEqual(["session-a"]);
    expect(next.panes.find((pane) => pane.id === "pane-b")?.sessionIds).toEqual(["session-b"]);
  });

  it("addSessionToPane appends and activates without duplicating", () => {
    const next = addSessionToPane(makeSinglePaneWorkspace(makePane("pane-a", ["session-a"])), "pane-a", "session-b");
    const deduped = addSessionToPane(next, "pane-a", "session-b");

    expect(next.panes[0].sessionIds).toEqual(["session-a", "session-b"]);
    expect(next.panes[0].activeSessionId).toBe("session-b");
    expect(deduped.panes[0].sessionIds).toEqual(["session-a", "session-b"]);
  });

  it("closeSessionTab removes a session and selects the next tab", () => {
    const next = closeSessionTab(
      makeSinglePaneWorkspace(makePane("pane-a", ["session-a", "session-b"], "session-a")),
      "pane-a",
      "session-a",
    );

    expect(next.panes[0].sessionIds).toEqual(["session-b"]);
    expect(next.panes[0].activeSessionId).toBe("session-b");
    expect(next.activePaneId).toBe("pane-a");
  });

  it("closeSessionTab removes the pane when its last session closes", () => {
    const next = closeSessionTab(makeSinglePaneWorkspace(makePane("pane-a", ["session-a"])), "pane-a", "session-a");

    expect(next.root).toBeNull();
    expect(next.panes).toEqual([]);
    expect(next.activePaneId).toBeNull();
  });

  it("splitPane creates an adjacent pane and moves the active tab into it", () => {
    const next = splitPane(
      makeSinglePaneWorkspace(makePane("pane-a", ["session-a", "session-b"], "session-b")),
      "pane-a",
      "row",
    );

    expect(next.panes).toHaveLength(2);
    expect(next.activePaneId).not.toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      sessionIds: ["session-a"],
      activeSessionId: "session-a",
    });
    expect(next.panes.find((pane) => pane.id !== "pane-a")).toMatchObject({
      sessionIds: ["session-b"],
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

  it("placeDraggedSession moves a tab between panes without creating duplicates", () => {
    const next = placeDraggedSession(
      makeSplitWorkspace(makePane("pane-a", ["session-a"]), makePane("pane-b", ["session-b", "session-a"])),
      "pane-a",
      "session-a",
      "pane-b",
      "tabs",
    );

    expect(next.panes).toHaveLength(1);
    expect(next.activePaneId).toBe("pane-b");
    expect(next.panes[0]).toMatchObject({
      id: "pane-b",
      sessionIds: ["session-b", "session-a"],
      activeSessionId: "session-a",
    });
    expect(next.root).toEqual({
      type: "pane",
      paneId: "pane-b",
    });
  });

  it("placeDraggedSession reorders tabs within a pane when dropped at a specific index", () => {
    const next = placeDraggedSession(
      makeSinglePaneWorkspace(makePane("pane-a", ["session-a", "session-b", "session-c"])),
      "pane-a",
      "session-a",
      "pane-a",
      "tabs",
      3,
    );

    expect(next.panes[0]).toMatchObject({
      id: "pane-a",
      sessionIds: ["session-b", "session-c", "session-a"],
      activeSessionId: "session-a",
    });
  });

  it("placeDraggedSession inserts a dragged tab into the requested position in another pane", () => {
    const next = placeDraggedSession(
      makeSplitWorkspace(makePane("pane-a", ["session-a"]), makePane("pane-b", ["session-b", "session-c"])),
      "pane-a",
      "session-a",
      "pane-b",
      "tabs",
      1,
    );

    expect(next.activePaneId).toBe("pane-b");
    expect(next.panes[0]).toMatchObject({
      id: "pane-b",
      sessionIds: ["session-b", "session-a", "session-c"],
      activeSessionId: "session-a",
    });
  });

  it("updateSplitRatio changes the selected split ratio and getSplitRatio reads it back", () => {
    const workspace = makeSplitWorkspace(makePane("pane-a", ["session-a"]), makePane("pane-b", ["session-b"]));

    const next = updateSplitRatio(workspace, "split-1", 0.75);

    expect(getSplitRatio(next.root, "split-1")).toBe(0.75);
  });

  it("reconcileWorkspaceState prunes missing sessions and recreates an initial pane when needed", () => {
    const pruned = reconcileWorkspaceState(
      makeSinglePaneWorkspace(makePane("pane-a", ["session-a", "session-b"], "session-b")),
      [makeSession("session-a")],
    );

    expect(pruned.panes[0].sessionIds).toEqual(["session-a"]);
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
    expect(rebuilt.panes[0].sessionIds).toEqual(["session-c"]);
    expect(rebuilt.root).toEqual({
      type: "pane",
      paneId: rebuilt.panes[0].id,
    });
  });
});
