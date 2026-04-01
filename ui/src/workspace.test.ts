import type { Session } from "./types";
import {
  addWorkspaceTabToPane,
  activatePane,
  closeWorkspaceTab,
  createPane,
  dockControlPanelAtWorkspaceEdge,
  openCanvasInWorkspaceState,
  ensureControlPanelInWorkspaceState,
  findNearestSessionPaneId,
  findWorkspacePaneIdForSession,
  openControlPanelInWorkspaceState,
  getSplitRatio,
  openDiffPreviewInWorkspaceState,
  openFilesystemInWorkspaceState,
  openGitStatusInWorkspaceState,
  openInstructionDebuggerInWorkspaceState,
  openOrchestratorCanvasInWorkspaceState,
  openOrchestratorListInWorkspaceState,
  openProjectListInWorkspaceState,
  openSessionInWorkspaceState,
  openSessionListInWorkspaceState,
  openSourceInWorkspaceState,
  placeSessionDropInWorkspaceState,
  placeDraggedTab,
  placeExternalTab,
  reconcileWorkspaceState,
  removeCanvasSessionCard,
  rescopeControlSurfacePane,
  setCanvasZoom,
  setPaneSourcePath,
  splitPane,
  updateGitDiffPreviewTabInWorkspaceState,
  updateSplitRatio,
  upsertCanvasSessionCard,
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

function makeSourceTab(
  id: string,
  path: string | null,
  originSessionId: string | null,
): WorkspaceTab {
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

function makeSessionListTab(
  id: string,
  originSessionId: string | null,
  originProjectId: string | null = null,
): WorkspaceTab {
  return {
    id,
    kind: "sessionList",
    originSessionId,
    ...(originProjectId ? { originProjectId } : {}),
  };
}

function makeProjectListTab(
  id: string,
  originSessionId: string | null,
  originProjectId: string | null = null,
): WorkspaceTab {
  return {
    id,
    kind: "projectList",
    originSessionId,
    ...(originProjectId ? { originProjectId } : {}),
  };
}

function makeControlPanelTab(
  id: string,
  originSessionId: string | null,
): WorkspaceTab {
  return {
    id,
    kind: "controlPanel",
    originSessionId,
  };
}

function makeOrchestratorListTab(
  id: string,
  originSessionId: string | null,
  originProjectId: string | null = null,
): WorkspaceTab {
  return {
    id,
    kind: "orchestratorList",
    originSessionId,
    ...(originProjectId ? { originProjectId } : {}),
  };
}

function makeCanvasTab(
  id: string,
  cards: Array<{ sessionId: string; x: number; y: number }>,
  originSessionId: string | null,
  originProjectId: string | null = null,
  zoom?: number,
): WorkspaceTab {
  return {
    id,
    kind: "canvas",
    cards,
    ...(typeof zoom === "number" ? { zoom } : {}),
    originSessionId,
    ...(originProjectId ? { originProjectId } : {}),
  };
}

function makeOrchestratorCanvasTab(
  id: string,
  originSessionId: string | null,
  options: {
    originProjectId?: string | null;
    startMode?: "new";
    templateId?: string | null;
  } = {},
): WorkspaceTab {
  return {
    id,
    kind: "orchestratorCanvas",
    originSessionId,
    ...(options.originProjectId
      ? { originProjectId: options.originProjectId }
      : {}),
    ...(options.templateId ? { templateId: options.templateId } : {}),
    ...(options.startMode ? { startMode: options.startMode } : {}),
  };
}

function makeInstructionDebuggerTab(
  id: string,
  workdir: string | null,
  originSessionId: string | null,
): WorkspaceTab {
  return {
    id,
    kind: "instructionDebugger",
    workdir,
    originSessionId,
  };
}

function makeDiffPreviewTab(
  id: string,
  diffMessageId: string,
  filePath: string | null,
  originSessionId: string | null,
  changeSetId: string | null = null,
): WorkspaceTab {
  return {
    id,
    kind: "diffPreview",
    changeType: "edit",
    ...(changeSetId ? { changeSetId } : {}),
    diff: "-before\n+after",
    diffMessageId,
    filePath,
    language: "typescript",
    originSessionId,
    summary: "Updated file",
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

  it("activatePane is a no-op when the pane and tab are already active", () => {
    const pane = makePane("pane-a", [makeSessionTab("tab-a", "session-a")], {
      activeTabId: "tab-a",
    });
    const workspace = makeSinglePaneWorkspace(pane);

    const next = activatePane(workspace, "pane-a");

    expect(next).toBe(workspace);
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

  it("openCanvasInWorkspaceState opens a canvas tab and reuses the same tab later", () => {
    const opened = openCanvasInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
      "pane-a",
      "session-a",
      "project-a",
    );

    expect(opened.panes[0]?.tabs).toEqual([
      makeSessionTab("tab-a", "session-a"),
      {
        id: expect.any(String),
        kind: "canvas",
        cards: [],
        originSessionId: "session-a",
        originProjectId: "project-a",
      },
    ]);

    const reused = openCanvasInWorkspaceState(opened, "pane-a", null, null);
    const canvasTab = opened.panes[0]?.tabs[1];

    expect(reused.activePaneId).toBe("pane-a");
    expect(reused.panes[0]?.activeTabId).toBe(canvasTab?.id ?? null);
    expect(reused.panes[0]?.tabs).toHaveLength(2);
  });

  it("openCanvasInWorkspaceState moves the existing canvas to the nearest session pane when launched from a control surface", () => {
    const controlPane = makePane(
      "pane-control",
      [makeControlPanelTab("control-a", null)],
      {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      },
    );
    const targetSessionPane = makePane(
      "pane-target",
      [makeSessionTab("tab-target", "session-target")],
      {
        activeTabId: "tab-target",
        activeSessionId: "session-target",
        viewMode: "session",
      },
    );
    const remoteCanvasPane = makePane(
      "pane-canvas",
      [
        makeSessionTab("tab-review", "session-review"),
        makeCanvasTab("canvas-a", [], "session-review", "project-review"),
      ],
      {
        activeTabId: "canvas-a",
        activeSessionId: "session-review",
        viewMode: "canvas",
      },
    );
    const workspace: WorkspaceState = {
      root: {
        id: "split-root",
        type: "split",
        direction: "row",
        ratio: 0.22,
        first: {
          type: "pane",
          paneId: controlPane.id,
        },
        second: {
          id: "split-content",
          type: "split",
          direction: "row",
          ratio: 0.5,
          first: {
            type: "pane",
            paneId: targetSessionPane.id,
          },
          second: {
            type: "pane",
            paneId: remoteCanvasPane.id,
          },
        },
      },
      panes: [controlPane, targetSessionPane, remoteCanvasPane],
      activePaneId: controlPane.id,
    };

    const next = openCanvasInWorkspaceState(
      workspace,
      controlPane.id,
      "session-target",
      "project-target",
    );

    expect(next.activePaneId).toBe(targetSessionPane.id);
    expect(next.panes.find((pane) => pane.id === controlPane.id)?.tabs).toEqual(
      [makeControlPanelTab("control-a", null)],
    );
    expect(
      next.panes.find((pane) => pane.id === targetSessionPane.id)?.tabs,
    ).toEqual([
      makeSessionTab("tab-target", "session-target"),
      {
        id: "canvas-a",
        kind: "canvas",
        cards: [],
        originSessionId: "session-target",
        originProjectId: "project-target",
      },
    ]);
    expect(
      next.panes.find((pane) => pane.id === targetSessionPane.id)?.activeTabId,
    ).toBe("canvas-a");
    expect(
      next.panes.find((pane) => pane.id === targetSessionPane.id)?.viewMode,
    ).toBe("canvas");
    expect(
      next.panes.find((pane) => pane.id === remoteCanvasPane.id)?.tabs,
    ).toEqual([makeSessionTab("tab-review", "session-review")]);
  });

  it("openOrchestratorListInWorkspaceState opens a reusable orchestrator library tab", () => {
    const opened = openOrchestratorListInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
      "pane-a",
      "session-a",
      "project-a",
    );

    expect(opened.panes[0]?.tabs).toEqual([
      makeSessionTab("tab-a", "session-a"),
      {
        id: expect.any(String),
        kind: "orchestratorList",
        originSessionId: "session-a",
        originProjectId: "project-a",
      },
    ]);

    const reused = openOrchestratorListInWorkspaceState(
      opened,
      "pane-a",
      null,
      null,
    );
    const orchestratorTab = opened.panes[0]?.tabs[1];

    expect(reused.activePaneId).toBe("pane-a");
    expect(reused.panes[0]?.activeTabId).toBe(orchestratorTab?.id ?? null);
    expect(reused.panes[0]?.tabs).toHaveLength(2);
  });

  it("openOrchestratorListInWorkspaceState reuses the library tab in the origin session pane", () => {
    const controlPane = makePane(
      "pane-control",
      [makeControlPanelTab("control-a", null)],
      {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      },
    );
    const sessionPaneA = makePane(
      "pane-session-a",
      [makeSessionTab("tab-a", "session-a")],
      {
        activeTabId: "tab-a",
        activeSessionId: "session-a",
        viewMode: "session",
      },
    );
    const sessionPaneB = makePane(
      "pane-session-b",
      [makeSessionTab("tab-b", "session-b")],
      {
        activeTabId: "tab-b",
        activeSessionId: "session-b",
        viewMode: "session",
      },
    );
    const workspace = {
      root: {
        id: "split-root",
        type: "split" as const,
        direction: "row" as const,
        ratio: 0.24,
        first: {
          type: "pane" as const,
          paneId: controlPane.id,
        },
        second: {
          id: "split-content",
          type: "split" as const,
          direction: "row" as const,
          ratio: 0.5,
          first: {
            type: "pane" as const,
            paneId: sessionPaneA.id,
          },
          second: {
            type: "pane" as const,
            paneId: sessionPaneB.id,
          },
        },
      },
      panes: [
        controlPane,
        {
          ...sessionPaneA,
          tabs: [
            makeSessionTab("tab-a", "session-a"),
            makeOrchestratorListTab(
              "orchestrators-a",
              "session-a",
              "project-a",
            ),
          ],
          activeTabId: "tab-a",
        },
        sessionPaneB,
      ],
      activePaneId: controlPane.id,
    };
    const existingOrchestratorTab = workspace.panes[1]?.tabs[1];
    if (
      !existingOrchestratorTab ||
      existingOrchestratorTab.kind !== "orchestratorList"
    ) {
      throw new Error("Expected existing orchestrator list tab");
    }

    const next = openOrchestratorListInWorkspaceState(
      workspace,
      controlPane.id,
      "session-b",
      "project-b",
    );

    expect(next.activePaneId).toBe(sessionPaneB.id);
    expect(
      next.panes.find((pane) => pane.id === sessionPaneA.id)?.tabs,
    ).toEqual([makeSessionTab("tab-a", "session-a")]);
    expect(
      next.panes.find((pane) => pane.id === sessionPaneB.id)?.tabs,
    ).toEqual([
      makeSessionTab("tab-b", "session-b"),
      {
        ...existingOrchestratorTab,
        originSessionId: "session-b",
        originProjectId: "project-b",
      },
    ]);
    expect(
      next.panes.find((pane) => pane.id === sessionPaneB.id)?.activeTabId,
    ).toBe(existingOrchestratorTab.id);
  });

  it("openOrchestratorListInWorkspaceState refreshes origin metadata when the tab is already in the target pane", () => {
    const sessionPane = makePane(
      "pane-session",
      [
        makeSessionTab("tab-session", "session-a"),
        makeOrchestratorListTab("orchestrators-a", null, "project-stale"),
      ],
      {
        activeTabId: "tab-session",
        activeSessionId: "session-a",
        viewMode: "session",
      },
    );
    const workspace = makeSinglePaneWorkspace(sessionPane);

    const next = openOrchestratorListInWorkspaceState(
      workspace,
      sessionPane.id,
      "session-a",
      "project-a",
    );

    expect(next.activePaneId).toBe(sessionPane.id);
    expect(next.panes[0]?.tabs).toEqual([
      makeSessionTab("tab-session", "session-a"),
      {
        id: "orchestrators-a",
        kind: "orchestratorList",
        originSessionId: "session-a",
        originProjectId: "project-a",
      },
    ]);
    expect(next.panes[0]?.activeTabId).toBe("orchestrators-a");
  });

  it("openOrchestratorCanvasInWorkspaceState creates a dedicated canvas tab for new drafts", () => {
    const next = openOrchestratorCanvasInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
      "pane-a",
      "session-a",
      "project-a",
      { startMode: "new" },
    );

    expect(next.panes[0]?.tabs).toEqual([
      makeSessionTab("tab-a", "session-a"),
      {
        id: expect.any(String),
        kind: "orchestratorCanvas",
        originSessionId: "session-a",
        originProjectId: "project-a",
        startMode: "new",
      },
    ]);
    expect(next.panes[0]).toMatchObject({
      activeSessionId: "session-a",
      viewMode: "orchestratorCanvas",
    });
  });

  it("openOrchestratorCanvasInWorkspaceState opens in the pane for the origin session when launched from the control panel", () => {
    const controlPane = makePane(
      "pane-control",
      [makeControlPanelTab("control-a", null)],
      {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      },
    );
    const sessionPaneA = makePane(
      "pane-session-a",
      [makeSessionTab("tab-a", "session-a")],
      {
        activeTabId: "tab-a",
        activeSessionId: "session-a",
        viewMode: "session",
      },
    );
    const sessionPaneB = makePane(
      "pane-session-b",
      [makeSessionTab("tab-b", "session-b")],
      {
        activeTabId: "tab-b",
        activeSessionId: "session-b",
        viewMode: "session",
      },
    );
    const workspace = {
      root: {
        id: "split-root",
        type: "split" as const,
        direction: "row" as const,
        ratio: 0.24,
        first: {
          type: "pane" as const,
          paneId: controlPane.id,
        },
        second: {
          id: "split-content",
          type: "split" as const,
          direction: "row" as const,
          ratio: 0.5,
          first: {
            type: "pane" as const,
            paneId: sessionPaneA.id,
          },
          second: {
            type: "pane" as const,
            paneId: sessionPaneB.id,
          },
        },
      },
      panes: [controlPane, sessionPaneA, sessionPaneB],
      activePaneId: controlPane.id,
    };

    const next = openOrchestratorCanvasInWorkspaceState(
      workspace,
      controlPane.id,
      "session-b",
      "project-b",
      { templateId: "template-b" },
    );

    expect(next.activePaneId).toBe(sessionPaneB.id);
    expect(next.panes.find((pane) => pane.id === controlPane.id)).toMatchObject(
      {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      },
    );
    expect(
      next.panes.find((pane) => pane.id === sessionPaneA.id)?.tabs,
    ).toEqual([makeSessionTab("tab-a", "session-a")]);
    expect(
      next.panes.find((pane) => pane.id === sessionPaneB.id)?.tabs,
    ).toEqual([
      makeSessionTab("tab-b", "session-b"),
      {
        id: expect.any(String),
        kind: "orchestratorCanvas",
        originSessionId: "session-b",
        originProjectId: "project-b",
        templateId: "template-b",
      },
    ]);
    expect(
      next.panes.find((pane) => pane.id === sessionPaneB.id),
    ).toMatchObject({
      activeSessionId: "session-b",
      activeTabId: expect.any(String),
      viewMode: "orchestratorCanvas",
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
    expect(next.panes.find((pane) => pane.id === "pane-a")?.activeTabId).toBe(
      "tab-a",
    );
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      makeSessionTab("tab-b", "session-b"),
    ]);
  });

  it("openSessionInWorkspaceState moves an existing session to the nearest session pane when launched from a control surface", () => {
    const controlPane = makePane(
      "pane-control",
      [makeControlPanelTab("control-a", null)],
      {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      },
    );
    const targetSessionPane = makePane(
      "pane-target",
      [makeSessionTab("tab-target", "session-target")],
      {
        activeTabId: "tab-target",
        activeSessionId: "session-target",
        viewMode: "session",
      },
    );
    const remoteSessionPane = makePane(
      "pane-remote",
      [makeSessionTab("tab-review", "session-review")],
      {
        activeTabId: "tab-review",
        activeSessionId: "session-review",
        viewMode: "session",
      },
    );
    const workspace: WorkspaceState = {
      root: {
        id: "split-root",
        type: "split",
        direction: "row",
        ratio: 0.22,
        first: {
          type: "pane",
          paneId: controlPane.id,
        },
        second: {
          id: "split-content",
          type: "split",
          direction: "row",
          ratio: 0.5,
          first: {
            type: "pane",
            paneId: targetSessionPane.id,
          },
          second: {
            type: "pane",
            paneId: remoteSessionPane.id,
          },
        },
      },
      panes: [controlPane, targetSessionPane, remoteSessionPane],
      activePaneId: controlPane.id,
    };

    const next = openSessionInWorkspaceState(
      workspace,
      "session-review",
      controlPane.id,
    );

    expect(next.activePaneId).toBe(targetSessionPane.id);
    expect(next.panes.find((pane) => pane.id === controlPane.id)?.tabs).toEqual(
      [makeControlPanelTab("control-a", null)],
    );
    expect(
      next.panes.find((pane) => pane.id === targetSessionPane.id)?.tabs,
    ).toEqual([
      makeSessionTab("tab-target", "session-target"),
      {
        id: "tab-review",
        kind: "session",
        sessionId: "session-review",
      },
    ]);
    expect(
      next.panes.find((pane) => pane.id === targetSessionPane.id)?.activeTabId,
    ).toBe("tab-review");
    expect(
      next.panes.find((pane) => pane.id === targetSessionPane.id)
        ?.activeSessionId,
    ).toBe("session-review");
    expect(next.panes.some((pane) => pane.id === remoteSessionPane.id)).toBe(
      false,
    );
  });

  it("openSessionInWorkspaceState prefers the first content pane to the right of a control surface", () => {
    const leftSessionPane = makePane(
      "pane-left",
      [makeSessionTab("tab-left", "session-left")],
      {
        activeTabId: "tab-left",
        activeSessionId: "session-left",
        viewMode: "session",
      },
    );
    const controlPane = makePane(
      "pane-control",
      [makeSessionListTab("sessions-a", null)],
      {
        activeTabId: "sessions-a",
        activeSessionId: null,
        viewMode: "sessionList",
      },
    );
    const rightSessionPane = makePane(
      "pane-right",
      [makeSessionTab("tab-right", "session-right")],
      {
        activeTabId: "tab-right",
        activeSessionId: "session-right",
        viewMode: "session",
      },
    );
    const workspace: WorkspaceState = {
      root: {
        id: "split-root",
        type: "split",
        direction: "row",
        ratio: 0.5,
        first: {
          id: "split-left",
          type: "split",
          direction: "row",
          ratio: 0.5,
          first: { type: "pane", paneId: leftSessionPane.id },
          second: { type: "pane", paneId: controlPane.id },
        },
        second: {
          type: "pane",
          paneId: rightSessionPane.id,
        },
      },
      panes: [leftSessionPane, controlPane, rightSessionPane],
      activePaneId: controlPane.id,
    };

    const next = openSessionInWorkspaceState(
      workspace,
      "session-new",
      controlPane.id,
    );

    expect(next.activePaneId).toBe(rightSessionPane.id);
    expect(
      next.panes.find((pane) => pane.id === leftSessionPane.id)?.tabs,
    ).toEqual([makeSessionTab("tab-left", "session-left")]);
    expect(next.panes.find((pane) => pane.id === controlPane.id)?.tabs).toEqual(
      [makeSessionListTab("sessions-a", null)],
    );
    expect(
      next.panes.find((pane) => pane.id === rightSessionPane.id)?.tabs,
    ).toEqual([
      makeSessionTab("tab-right", "session-right"),
      {
        id: expect.any(String),
        kind: "session",
        sessionId: "session-new",
      },
    ]);
    expect(
      next.panes.find((pane) => pane.id === rightSessionPane.id)
        ?.activeSessionId,
    ).toBe("session-new");
  });

  it("openSessionInWorkspaceState creates a new pane when only a control surface and one content pane exist", () => {
    const controlPane = makePane(
      "pane-control",
      [makeControlPanelTab("control-a", null)],
      {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      },
    );
    const sessionPane = makePane(
      "pane-session",
      [makeSessionTab("tab-session", "session-a")],
      {
        activeTabId: "tab-session",
        activeSessionId: "session-a",
        viewMode: "session",
      },
    );

    const next = openSessionInWorkspaceState(
      makeSplitWorkspace(controlPane, sessionPane, controlPane.id),
      "session-b",
      controlPane.id,
    );

    const newSessionPane = next.panes.find(
      (pane) =>
        pane.id !== controlPane.id &&
        pane.id !== sessionPane.id &&
        pane.tabs.some(
          (tab) => tab.kind === "session" && tab.sessionId === "session-b",
        ),
    );

    expect(next.panes).toHaveLength(3);
    expect(next.activePaneId).toBe(newSessionPane?.id ?? null);
    expect(next.panes.find((pane) => pane.id === controlPane.id)?.tabs).toEqual(
      [makeControlPanelTab("control-a", null)],
    );
    expect(next.panes.find((pane) => pane.id === sessionPane.id)?.tabs).toEqual(
      [makeSessionTab("tab-session", "session-a")],
    );
    expect(newSessionPane).toMatchObject({
      tabs: [
        {
          id: expect.any(String),
          kind: "session",
          sessionId: "session-b",
        },
      ],
      activeSessionId: "session-b",
      viewMode: "session",
    });
  });

  it("placeSessionDropInWorkspaceState adds a dropped session to the target tab rail", () => {
    const paneA = makePane("pane-a", [makeSessionTab("tab-a", "session-a")]);
    const paneB = makePane("pane-b", [
      makeSessionListTab("tab-sessions", null),
    ]);

    const next = placeSessionDropInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "session-b",
      paneB.id,
      "tabs",
    );

    const targetPane = next.panes.find((pane) => pane.id === paneB.id);
    expect(
      targetPane?.tabs.some(
        (tab) => tab.kind === "session" && tab.sessionId === "session-b",
      ),
    ).toBe(true);
    expect(next.activePaneId).toBe(paneB.id);
  });

  it("placeSessionDropInWorkspaceState inserts a newly opened session at the requested tab index", () => {
    const paneA = makePane("pane-a", [makeSessionTab("tab-a", "session-a")]);
    const paneB = makePane("pane-b", [
      makeSessionTab("tab-b", "session-b"),
      makeSessionTab("tab-c", "session-c"),
    ]);

    const next = placeSessionDropInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "session-new",
      paneB.id,
      "tabs",
      1,
    );

    expect(next.panes.find((pane) => pane.id === paneB.id)?.tabs).toEqual([
      makeSessionTab("tab-b", "session-b"),
      {
        id: expect.any(String),
        kind: "session",
        sessionId: "session-new",
      },
      makeSessionTab("tab-c", "session-c"),
    ]);
    expect(
      next.panes.find((pane) => pane.id === paneB.id)?.activeSessionId,
    ).toBe("session-new");
  });

  it("placeSessionDropInWorkspaceState moves an already open session to the requested tab index", () => {
    const paneA = makePane("pane-a", [makeSessionTab("tab-a", "session-a")]);
    const paneB = makePane("pane-b", [
      makeSessionTab("tab-b", "session-b"),
      makeSessionTab("tab-c", "session-c"),
    ]);

    const next = placeSessionDropInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneA.id),
      "session-a",
      paneB.id,
      "tabs",
      1,
    );

    expect(next.panes.find((pane) => pane.id === paneB.id)?.tabs).toEqual([
      makeSessionTab("tab-b", "session-b"),
      {
        id: "tab-a",
        kind: "session",
        sessionId: "session-a",
      },
      makeSessionTab("tab-c", "session-c"),
    ]);
    expect(
      next.panes
        .flatMap((pane) => pane.tabs)
        .filter(
          (tab) => tab.kind === "session" && tab.sessionId === "session-a",
        ),
    ).toHaveLength(1);
    expect(
      next.panes.find((pane) => pane.id === paneB.id)?.activeSessionId,
    ).toBe("session-a");
  });

  it("placeSessionDropInWorkspaceState reorders an already open session within the same pane", () => {
    const pane = makePane("pane-a", [
      makeSessionTab("tab-a", "session-a"),
      makeSessionTab("tab-b", "session-b"),
      makeSessionTab("tab-c", "session-c"),
    ]);

    const next = placeSessionDropInWorkspaceState(
      makeSinglePaneWorkspace(pane),
      "session-a",
      pane.id,
      "tabs",
      2,
    );

    expect(next.panes[0]?.tabs).toEqual([
      makeSessionTab("tab-b", "session-b"),
      {
        id: "tab-a",
        kind: "session",
        sessionId: "session-a",
      },
      makeSessionTab("tab-c", "session-c"),
    ]);
    expect(
      next.panes[0]?.tabs.filter(
        (tab) => tab.kind === "session" && tab.sessionId === "session-a",
      ),
    ).toHaveLength(1);
    expect(next.panes[0]?.activeSessionId).toBe("session-a");
  });

  it("placeSessionDropInWorkspaceState creates an adjacent pane for a side drop", () => {
    const paneA = makePane("pane-a", [makeSessionTab("tab-a", "session-a")]);
    const paneB = makePane("pane-b", [
      makeSessionListTab("tab-sessions", null),
    ]);

    const next = placeSessionDropInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "session-b",
      paneB.id,
      "right",
    );

    expect(
      next.panes.some((pane) =>
        pane.tabs.some(
          (tab) => tab.kind === "session" && tab.sessionId === "session-b",
        ),
      ),
    ).toBe(true);
    expect(next.panes).toHaveLength(3);
  });

  it("openInstructionDebuggerInWorkspaceState focuses the existing debugger tab for the same session", () => {
    const paneA = makePane("pane-a", [
      makeSessionTab("tab-session", "session-a"),
      makeInstructionDebuggerTab(
        "tab-instructions",
        "/tmp/project",
        "session-a",
      ),
    ]);
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openInstructionDebuggerInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "/tmp/project",
      paneB.id,
      "session-a",
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")?.activeTabId).toBe(
      "tab-instructions",
    );
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      makeSessionTab("tab-b", "session-b"),
    ]);
  });

  it("dockControlPanelAtWorkspaceEdge preserves the resized control panel width", () => {
    const workspace = {
      root: {
        id: "split-1",
        type: "split" as const,
        direction: "row" as const,
        ratio: 0.14,
        first: {
          type: "pane" as const,
          paneId: "pane-control",
        },
        second: {
          type: "pane" as const,
          paneId: "pane-session",
        },
      },
      panes: [
        makePane("pane-control", [makeControlPanelTab("control-a", null)], {
          activeTabId: "control-a",
          activeSessionId: null,
          viewMode: "controlPanel",
        }),
        makePane("pane-session", [makeSessionTab("tab-a", "session-a")], {
          activeTabId: "tab-a",
          activeSessionId: "session-a",
          viewMode: "session",
        }),
      ],
      activePaneId: "pane-session",
    };

    const next = dockControlPanelAtWorkspaceEdge(workspace, "left");

    expect(next.root).toMatchObject({
      id: "split-1",
      type: "split",
      direction: "row",
      ratio: 0.14,
    });
  });

  it("dockControlPanelAtWorkspaceEdge preserves control panel width when moving sides", () => {
    const workspace = {
      root: {
        id: "split-1",
        type: "split" as const,
        direction: "row" as const,
        ratio: 0.14,
        first: {
          type: "pane" as const,
          paneId: "pane-control",
        },
        second: {
          type: "pane" as const,
          paneId: "pane-session",
        },
      },
      panes: [
        makePane("pane-control", [makeControlPanelTab("control-a", null)], {
          activeTabId: "control-a",
          activeSessionId: null,
          viewMode: "controlPanel",
        }),
        makePane("pane-session", [makeSessionTab("tab-a", "session-a")], {
          activeTabId: "tab-a",
          activeSessionId: "session-a",
          viewMode: "session",
        }),
      ],
      activePaneId: "pane-session",
    };

    const next = dockControlPanelAtWorkspaceEdge(workspace, "right");

    expect(next.root).toMatchObject({
      id: "split-1",
      type: "split",
      direction: "row",
      ratio: 0.86,
    });
  });

  it("findWorkspacePaneIdForSession returns the pane that owns the session tab", () => {
    const paneA = makePane("pane-a", [makeSessionTab("tab-a", "session-a")]);
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const paneId = findWorkspacePaneIdForSession(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "session-b",
    );

    expect(paneId).toBe("pane-b");
    expect(
      findWorkspacePaneIdForSession(
        makeSplitWorkspace(paneA, paneB),
        "session-c",
      ),
    ).toBeNull();
  });

  it("rescopeControlSurfacePane updates git status tabs to the new session context", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane(
        "pane-a",
        [makeGitStatusTab("git-a", "/repo/old", "session-a")],
        {
          activeTabId: "git-a",
          activeSessionId: "session-a",
          viewMode: "gitStatus",
        },
      ),
    );

    const next = rescopeControlSurfacePane(
      workspace,
      "pane-a",
      "session-b",
      "project-b",
      "/repo/new",
    );

    expect(next.panes[0]?.tabs).toEqual([
      {
        id: "git-a",
        kind: "gitStatus",
        workdir: "/repo/new",
        originSessionId: "session-b",
        originProjectId: "project-b",
      },
    ]);
    expect(next.panes[0]).toMatchObject({
      activeSessionId: "session-b",
      activeTabId: "git-a",
      viewMode: "gitStatus",
    });
  });

  it("rescopeControlSurfacePane updates filesystem tabs to the new session context", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane(
        "pane-a",
        [makeFilesystemTab("fs-a", "/repo/old", "session-a")],
        {
          activeTabId: "fs-a",
          activeSessionId: "session-a",
          viewMode: "filesystem",
        },
      ),
    );

    const next = rescopeControlSurfacePane(
      workspace,
      "pane-a",
      "session-b",
      "project-b",
      "/repo/new",
    );

    expect(next.panes[0]?.tabs).toEqual([
      {
        id: "fs-a",
        kind: "filesystem",
        rootPath: "/repo/new",
        originSessionId: "session-b",
        originProjectId: "project-b",
      },
    ]);
    expect(next.panes[0]).toMatchObject({
      activeSessionId: "session-b",
      activeTabId: "fs-a",
      viewMode: "filesystem",
    });
  });

  it("rescopeControlSurfacePane updates origin-only tabs without changing their kind", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane("pane-a", [makeControlPanelTab("control-a", "session-a")], {
        activeTabId: "control-a",
        activeSessionId: "session-a",
        viewMode: "controlPanel",
      }),
    );

    const next = rescopeControlSurfacePane(
      workspace,
      "pane-a",
      "session-b",
      "project-b",
      "/repo/new",
    );

    expect(next.panes[0]?.tabs).toEqual([
      {
        id: "control-a",
        kind: "controlPanel",
        originSessionId: "session-b",
        originProjectId: "project-b",
      },
    ]);
    expect(next.panes[0]).toMatchObject({
      activeSessionId: "session-b",
      activeTabId: "control-a",
      viewMode: "controlPanel",
    });
  });

  it("rescopeControlSurfacePane is a no-op when the pane is missing", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane("pane-a", [makeControlPanelTab("control-a", "session-a")], {
        activeTabId: "control-a",
        activeSessionId: "session-a",
        viewMode: "controlPanel",
      }),
    );

    expect(
      rescopeControlSurfacePane(
        workspace,
        "pane-missing",
        "session-b",
        "project-b",
        "/repo/new",
      ),
    ).toBe(workspace);
  });

  it("rescopeControlSurfacePane is a no-op when the pane has no active tab", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane("pane-a", [], {
        activeTabId: null,
        activeSessionId: null,
        viewMode: "controlPanel",
      }),
    );

    expect(
      rescopeControlSurfacePane(
        workspace,
        "pane-a",
        "session-b",
        "project-b",
        "/repo/new",
      ),
    ).toBe(workspace);
  });

  it("findNearestSessionPaneId prefers the nearest session pane on the left", () => {
    const leftPane = makePane(
      "pane-left",
      [makeSessionTab("tab-left", "session-left")],
      {
        activeTabId: "tab-left",
        activeSessionId: "session-left",
        viewMode: "session",
      },
    );
    const middlePane = makePane(
      "pane-middle",
      [makeControlPanelTab("control-a", null)],
      {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      },
    );
    const rightPane = makePane(
      "pane-right",
      [makeSessionTab("tab-right", "session-right")],
      {
        activeTabId: "tab-right",
        activeSessionId: "session-right",
        viewMode: "session",
      },
    );
    const workspace: WorkspaceState = {
      root: {
        id: "split-root",
        type: "split",
        direction: "row",
        ratio: 0.5,
        first: { type: "pane", paneId: leftPane.id },
        second: {
          id: "split-right",
          type: "split",
          direction: "row",
          ratio: 0.5,
          first: { type: "pane", paneId: middlePane.id },
          second: { type: "pane", paneId: rightPane.id },
        },
      },
      panes: [leftPane, middlePane, rightPane],
      activePaneId: middlePane.id,
    };

    expect(findNearestSessionPaneId(workspace, middlePane.id)).toBe(
      leftPane.id,
    );
  });

  it("findNearestSessionPaneId falls back to the nearest session pane on the right", () => {
    const controlPane = makePane(
      "pane-control",
      [makeControlPanelTab("control-a", null)],
      {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      },
    );
    const sessionPane = makePane(
      "pane-session",
      [makeSessionTab("tab-session", "session-a")],
      {
        activeTabId: "tab-session",
        activeSessionId: "session-a",
        viewMode: "session",
      },
    );

    expect(
      findNearestSessionPaneId(
        makeSplitWorkspace(controlPane, sessionPane, controlPane.id),
        controlPane.id,
      ),
    ).toBe(sessionPane.id);
  });

  it("findNearestSessionPaneId returns null when no session panes exist", () => {
    const workspace = makeSplitWorkspace(
      makePane("pane-a", [makeControlPanelTab("control-a", null)], {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      }),
      makePane("pane-b", [makeGitStatusTab("git-a", "/repo", null)], {
        activeTabId: "git-a",
        activeSessionId: null,
        viewMode: "gitStatus",
      }),
      "pane-a",
    );

    expect(findNearestSessionPaneId(workspace, "pane-a")).toBeNull();
  });

  it("findNearestSessionPaneId returns null when the pane id is not in the workspace", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane("pane-a", [makeSessionTab("tab-a", "session-a")], {
        activeTabId: "tab-a",
        activeSessionId: "session-a",
        viewMode: "session",
      }),
    );

    expect(findNearestSessionPaneId(workspace, "pane-missing")).toBeNull();
  });

  it("addWorkspaceTabToPane appends and activates without duplicating by tab id", () => {
    const initial = makeSinglePaneWorkspace(
      makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
    );
    const next = addWorkspaceTabToPane(
      initial,
      "pane-a",
      makeSessionTab("tab-b", "session-b"),
    );
    const deduped = addWorkspaceTabToPane(
      next,
      "pane-a",
      next.panes[0].tabs[1],
    );

    expect(next.panes[0].tabs.map((tab) => tab.id)).toEqual(["tab-a", "tab-b"]);
    expect(next.panes[0].activeSessionId).toBe("session-b");
    expect(deduped.panes[0].tabs.map((tab) => tab.id)).toEqual([
      "tab-a",
      "tab-b",
    ]);
  });

  it("closeWorkspaceTab removes a tab and selects the next one", () => {
    const next = closeWorkspaceTab(
      makeSinglePaneWorkspace(
        makePane(
          "pane-a",
          [
            makeSessionTab("tab-a", "session-a"),
            makeSessionTab("tab-b", "session-b"),
          ],
          {
            activeTabId: "tab-a",
            activeSessionId: "session-a",
          },
        ),
      ),
      "pane-a",
      "tab-a",
    );

    expect(next.panes[0].tabs.map((tab) => tab.id)).toEqual(["tab-b"]);
    expect(next.panes[0].activeTabId).toBe("tab-b");
    expect(next.panes[0].activeSessionId).toBe("session-b");
    expect(next.activePaneId).toBe("pane-a");
  });

  it("closeWorkspaceTab selects the adjacent tab instead of jumping to the end", () => {
    const next = closeWorkspaceTab(
      makeSinglePaneWorkspace(
        makePane(
          "pane-a",
          [
            makeSessionTab("tab-a", "session-a"),
            makeSessionTab("tab-b", "session-b"),
            makeSessionTab("tab-c", "session-c"),
          ],
          {
            activeTabId: "tab-a",
            activeSessionId: "session-a",
          },
        ),
      ),
      "pane-a",
      "tab-a",
    );

    expect(next.panes[0].tabs.map((tab) => tab.id)).toEqual(["tab-b", "tab-c"]);
    expect(next.panes[0].activeTabId).toBe("tab-b");
    expect(next.panes[0].activeSessionId).toBe("session-b");
  });

  it("closeWorkspaceTab selects the following tab when closing a middle tab", () => {
    const next = closeWorkspaceTab(
      makeSinglePaneWorkspace(
        makePane(
          "pane-a",
          [
            makeSessionTab("tab-a", "session-a"),
            makeSessionTab("tab-b", "session-b"),
            makeSessionTab("tab-c", "session-c"),
          ],
          {
            activeTabId: "tab-b",
            activeSessionId: "session-b",
          },
        ),
      ),
      "pane-a",
      "tab-b",
    );

    expect(next.panes[0].tabs.map((tab) => tab.id)).toEqual(["tab-a", "tab-c"]);
    expect(next.panes[0].activeTabId).toBe("tab-c");
    expect(next.panes[0].activeSessionId).toBe("session-c");
  });

  it("closeWorkspaceTab removes the pane when its last tab closes", () => {
    const next = closeWorkspaceTab(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
      "pane-a",
      "tab-a",
    );

    expect(next.root).toBeNull();
    expect(next.panes).toEqual([]);
    expect(next.activePaneId).toBeNull();
  });

  it("openSourceInWorkspaceState opens a source tab in the current session pane", () => {
    const next = openSourceInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
      "/tmp/app.ts",
      "pane-a",
      "session-a",
    );

    expect(next.panes).toHaveLength(1);
    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes[0]?.tabs).toEqual([
      makeSessionTab("tab-a", "session-a"),
      {
        id: expect.any(String),
        kind: "source",
        path: "/tmp/app.ts",
        originSessionId: "session-a",
      },
    ]);
    expect(next.panes[0]).toMatchObject({
      activeSessionId: "session-a",
      viewMode: "source",
      sourcePath: "/tmp/app.ts",
    });
    expect(next.panes[0]?.activeTabId).toBe(next.panes[0]?.tabs[1]?.id ?? null);
  });

  it("openSourceInWorkspaceState opens a file from the control panel beside the session pane", () => {
    const next = openSourceInWorkspaceState(
      makeSplitWorkspace(
        makePane("pane-a", [makeControlPanelTab("control-a", null)], {
          activeTabId: "control-a",
          activeSessionId: null,
          viewMode: "controlPanel",
        }),
        makePane("pane-b", [makeSessionTab("tab-b", "session-b")]),
        "pane-a",
      ),
      "/tmp/app.ts",
      "pane-a",
      null,
    );
    const sourcePane = next.panes.find((pane) =>
      pane.tabs.some((tab) => tab.kind === "source"),
    );

    expect(next.panes).toHaveLength(3);
    expect(sourcePane).toBeTruthy();
    expect(next.activePaneId).toBe(sourcePane?.id ?? null);
    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      tabs: [makeControlPanelTab("control-a", null)],
      activeTabId: "control-a",
      viewMode: "controlPanel",
      activeSessionId: null,
    });
    expect(next.panes.find((pane) => pane.id === "pane-b")).toMatchObject({
      tabs: [makeSessionTab("tab-b", "session-b")],
      activeTabId: "tab-b",
      viewMode: "session",
      activeSessionId: "session-b",
    });
    expect(sourcePane).toMatchObject({
      tabs: [
        {
          id: expect.any(String),
          kind: "source",
          path: "/tmp/app.ts",
          originSessionId: null,
        },
      ],
      viewMode: "source",
      sourcePath: "/tmp/app.ts",
    });
  });

  it("openSourceInWorkspaceState opens a new tab instead of reusing an existing source tab when requested", () => {
    const next = openSourceInWorkspaceState(
      makeSplitWorkspace(
        makePane("pane-a", [makeControlPanelTab("control-a", null)], {
          activeTabId: "control-a",
          activeSessionId: null,
          viewMode: "controlPanel",
        }),
        makePane("pane-b", [makeSourceTab("source-a", "/tmp/app.ts", null)], {
          activeTabId: "source-a",
          activeSessionId: null,
          viewMode: "source",
          sourcePath: "/tmp/app.ts",
        }),
        "pane-a",
      ),
      "/tmp/app.ts",
      "pane-a",
      null,
      {
        openInNewTab: true,
      },
    );

    expect(next.panes).toHaveLength(2);
    expect(next.activePaneId).toBe("pane-b");
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      makeSourceTab("source-a", "/tmp/app.ts", null),
      {
        id: expect.any(String),
        kind: "source",
        path: "/tmp/app.ts",
        originSessionId: null,
      },
    ]);
    expect(next.panes.find((pane) => pane.id === "pane-b")).toMatchObject({
      viewMode: "source",
      sourcePath: "/tmp/app.ts",
      activeSessionId: null,
    });
  });

  it("openSourceInWorkspaceState opens a file from a filesystem pane in a separate source pane", () => {
    const next = openSourceInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane(
          "pane-a",
          [makeFilesystemTab("fs-a", "/tmp/project", "session-a")],
          {
            activeTabId: "fs-a",
            activeSessionId: "session-a",
            viewMode: "filesystem",
          },
        ),
      ),
      "/tmp/project/src/app.ts",
      "pane-a",
      "session-a",
    );

    expect(next.panes).toHaveLength(2);
    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      tabs: [makeFilesystemTab("fs-a", "/tmp/project", "session-a")],
      activeTabId: "fs-a",
      viewMode: "filesystem",
      activeSessionId: "session-a",
    });
    expect(next.panes.find((pane) => pane.id !== "pane-a")).toMatchObject({
      tabs: [
        {
          id: expect.any(String),
          kind: "source",
          path: "/tmp/project/src/app.ts",
          originSessionId: "session-a",
        },
      ],
      viewMode: "source",
      sourcePath: "/tmp/project/src/app.ts",
      activeSessionId: "session-a",
    });
  });

  it("openSourceInWorkspaceState moves an existing source tab into the current session pane", () => {
    const next = openSourceInWorkspaceState(
      makeSplitWorkspace(
        makePane(
          "pane-a",
          [makeSourceTab("source-a", "/tmp/app.ts", "session-a")],
          {
            activeTabId: "source-a",
            activeSessionId: "session-a",
            viewMode: "source",
            sourcePath: "/tmp/app.ts",
          },
        ),
        makePane("pane-b", [makeSessionTab("tab-b", "session-a")]),
        "pane-b",
      ),
      "/tmp/app.ts",
      "pane-b",
      "session-a",
    );

    expect(next.panes).toHaveLength(1);
    expect(next.activePaneId).toBe("pane-b");
    expect(next.panes[0]?.tabs).toEqual([
      makeSessionTab("tab-b", "session-a"),
      {
        id: "source-a",
        kind: "source",
        path: "/tmp/app.ts",
        originSessionId: "session-a",
      },
    ]);
    expect(next.panes[0]).toMatchObject({
      id: "pane-b",
      activeSessionId: "session-a",
      activeTabId: "source-a",
      viewMode: "source",
      sourcePath: "/tmp/app.ts",
    });
  });

  it("openSourceInWorkspaceState retargets an existing source tab to the requested line", () => {
    const next = openSourceInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane(
          "pane-a",
          [
            makeSessionTab("tab-a", "session-a"),
            makeSourceTab("source-a", "/tmp/app.ts", "session-a"),
          ],
          {
            activeTabId: "tab-a",
            activeSessionId: "session-a",
            viewMode: "session",
          },
        ),
      ),
      "/tmp/app.ts",
      "pane-a",
      "session-a",
      {
        line: 63,
      },
    );

    const sourceTab = next.panes[0]?.tabs.find((tab) => tab.id === "source-a");
    expect(sourceTab).toMatchObject({
      id: "source-a",
      kind: "source",
      path: "/tmp/app.ts",
      focusLineNumber: 63,
      focusToken: expect.any(String),
      originSessionId: "session-a",
    });
    expect(next.panes[0]).toMatchObject({
      activeSessionId: "session-a",
      activeTabId: "source-a",
      viewMode: "source",
      sourcePath: "/tmp/app.ts",
    });
  });
  it("openFilesystemInWorkspaceState creates a filesystem tab and switches the pane mode", () => {
    const next = openFilesystemInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
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
    const paneA = makePane(
      "pane-a",
      [makeFilesystemTab("fs-a", "/tmp/project", "session-a")],
      {
        activeTabId: "fs-a",
        activeSessionId: "session-a",
        viewMode: "filesystem",
      },
    );
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openFilesystemInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "/tmp/project",
      paneB.id,
      "session-b",
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")?.activeTabId).toBe(
      "fs-a",
    );
  });

  it("openGitStatusInWorkspaceState creates a git status tab and switches the pane mode", () => {
    const next = openGitStatusInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
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
    const paneA = makePane(
      "pane-a",
      [makeGitStatusTab("git-a", "/tmp/project", "session-a")],
      {
        activeTabId: "git-a",
        activeSessionId: "session-a",
        viewMode: "gitStatus",
      },
    );
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openGitStatusInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "/tmp/project",
      paneB.id,
      "session-b",
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")?.activeTabId).toBe(
      "git-a",
    );
  });

  it("openSessionListInWorkspaceState creates a sessions tab and switches the pane mode", () => {
    const next = openSessionListInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
      "pane-a",
      "session-a",
      "project-a",
    );

    expect(next.panes[0].tabs).toHaveLength(2);
    expect(next.panes[0].tabs[1]).toEqual({
      id: expect.any(String),
      kind: "sessionList",
      originSessionId: "session-a",
      originProjectId: "project-a",
    });
    expect(next.panes[0].activeTabId).toBe(next.panes[0].tabs[1]?.id ?? null);
    expect(next.panes[0].viewMode).toBe("sessionList");
    expect(next.panes[0].activeSessionId).toBe("session-a");
  });

  it("openSessionListInWorkspaceState moves the existing sessions tab into the new session pane", () => {
    const paneA = makePane(
      "pane-a",
      [makeSessionListTab("sessions-a", "session-a")],
      {
        activeTabId: "sessions-a",
        activeSessionId: "session-a",
        viewMode: "sessionList",
      },
    );
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openSessionListInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      paneB.id,
      "session-b",
      "project-b",
    );

    expect(next.activePaneId).toBe("pane-b");
    expect(next.panes.some((pane) => pane.id === "pane-a")).toBe(false);
    expect(next.panes.find((pane) => pane.id === "pane-b")).toMatchObject({
      activeTabId: "sessions-a",
      activeSessionId: "session-b",
      viewMode: "sessionList",
      tabs: [
        makeSessionTab("tab-b", "session-b"),
        makeSessionListTab("sessions-a", "session-b", "project-b"),
      ],
    });
  });

  it("openSessionListInWorkspaceState opens in the pane for the origin session when launched from the control panel", () => {
    const controlPane = makePane(
      "pane-control",
      [makeControlPanelTab("control-a", null)],
      {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      },
    );
    const sessionPaneA = makePane(
      "pane-session-a",
      [makeSessionTab("tab-a", "session-a")],
      {
        activeTabId: "tab-a",
        activeSessionId: "session-a",
        viewMode: "session",
      },
    );
    const sessionPaneB = makePane(
      "pane-session-b",
      [makeSessionTab("tab-b", "session-b")],
      {
        activeTabId: "tab-b",
        activeSessionId: "session-b",
        viewMode: "session",
      },
    );
    const workspace = {
      root: {
        id: "split-root",
        type: "split" as const,
        direction: "row" as const,
        ratio: 0.24,
        first: {
          type: "pane" as const,
          paneId: controlPane.id,
        },
        second: {
          id: "split-content",
          type: "split" as const,
          direction: "row" as const,
          ratio: 0.5,
          first: {
            type: "pane" as const,
            paneId: sessionPaneA.id,
          },
          second: {
            type: "pane" as const,
            paneId: sessionPaneB.id,
          },
        },
      },
      panes: [controlPane, sessionPaneA, sessionPaneB],
      activePaneId: controlPane.id,
    };

    const next = openSessionListInWorkspaceState(
      workspace,
      controlPane.id,
      "session-b",
      "project-b",
    );

    expect(next.activePaneId).toBe(sessionPaneB.id);
    expect(next.panes.find((pane) => pane.id === controlPane.id)).toMatchObject(
      {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      },
    );
    expect(
      next.panes.find((pane) => pane.id === sessionPaneA.id)?.tabs,
    ).toEqual([makeSessionTab("tab-a", "session-a")]);
    expect(
      next.panes.find((pane) => pane.id === sessionPaneB.id)?.tabs,
    ).toEqual([
      makeSessionTab("tab-b", "session-b"),
      {
        id: expect.any(String),
        kind: "sessionList",
        originSessionId: "session-b",
        originProjectId: "project-b",
      },
    ]);
    expect(
      next.panes.find((pane) => pane.id === sessionPaneB.id),
    ).toMatchObject({
      activeSessionId: "session-b",
      viewMode: "sessionList",
    });
  });

  it("openProjectListInWorkspaceState creates a projects tab and switches the pane mode", () => {
    const next = openProjectListInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
      "pane-a",
      "session-a",
      "project-a",
    );

    expect(next.panes[0].tabs).toHaveLength(2);
    expect(next.panes[0].tabs[1]).toEqual({
      id: expect.any(String),
      kind: "projectList",
      originSessionId: "session-a",
      originProjectId: "project-a",
    });
    expect(next.panes[0].activeTabId).toBe(next.panes[0].tabs[1]?.id ?? null);
    expect(next.panes[0].viewMode).toBe("projectList");
    expect(next.panes[0].activeSessionId).toBe("session-a");
  });

  it("openProjectListInWorkspaceState focuses the existing projects tab instead of duplicating it", () => {
    const paneA = makePane(
      "pane-a",
      [makeProjectListTab("projects-a", "session-a")],
      {
        activeTabId: "projects-a",
        activeSessionId: "session-a",
        viewMode: "projectList",
      },
    );
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openProjectListInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      paneB.id,
      "session-b",
      "project-b",
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      activeTabId: "projects-a",
      activeSessionId: "session-a",
      viewMode: "projectList",
    });
    expect(next.panes.find((pane) => pane.id === "pane-a")?.tabs).toEqual([
      makeProjectListTab("projects-a", "session-a"),
    ]);
  });

  it("openControlPanelInWorkspaceState creates a control panel pane and preserves session context", () => {
    const next = openControlPanelInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
      "pane-a",
      "session-a",
    );

    expect(next.panes).toHaveLength(2);
    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      tabs: [makeSessionTab("tab-a", "session-a")],
      activeTabId: "tab-a",
      activeSessionId: "session-a",
      viewMode: "session",
    });
    expect(next.panes.find((pane) => pane.id !== "pane-a")).toMatchObject({
      tabs: [
        {
          id: expect.any(String),
          kind: "controlPanel",
          originSessionId: "session-a",
        },
      ],
      viewMode: "controlPanel",
      activeSessionId: "session-a",
    });
    expect(next.root).toMatchObject({
      type: "split",
      direction: "row",
    });
  });

  it("openControlPanelInWorkspaceState focuses the existing control panel instead of duplicating it", () => {
    const paneA = makePane(
      "pane-a",
      [makeControlPanelTab("control-a", "session-a")],
      {
        activeTabId: "control-a",
        activeSessionId: "session-a",
        viewMode: "controlPanel",
      },
    );
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openControlPanelInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      paneB.id,
      "session-b",
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")?.activeTabId).toBe(
      "control-a",
    );
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      makeSessionTab("tab-b", "session-b"),
    ]);
  });

  it("ensureControlPanelInWorkspaceState creates a control panel pane for an empty workspace", () => {
    const next = ensureControlPanelInWorkspaceState({
      root: null,
      panes: [],
      activePaneId: null,
    });

    expect(next.panes).toHaveLength(1);
    expect(next.panes[0]).toMatchObject({
      tabs: [
        {
          kind: "controlPanel",
          originSessionId: null,
        },
      ],
      viewMode: "controlPanel",
      activeSessionId: null,
    });
    expect(next.activePaneId).toBe(next.panes[0].id);
  });

  it("openSessionInWorkspaceState opens beside the control panel instead of inside it", () => {
    const next = openSessionInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeControlPanelTab("control-a", null)], {
          activeTabId: "control-a",
          activeSessionId: null,
          viewMode: "controlPanel",
        }),
      ),
      "session-a",
      "pane-a",
    );

    expect(next.panes).toHaveLength(2);
    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      tabs: [makeControlPanelTab("control-a", null)],
      activeTabId: "control-a",
      viewMode: "controlPanel",
      activeSessionId: null,
    });
    expect(next.panes.find((pane) => pane.id !== "pane-a")).toMatchObject({
      tabs: [
        {
          kind: "session",
          sessionId: "session-a",
        },
      ],
      activeSessionId: "session-a",
      viewMode: "session",
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

  it("openDiffPreviewInWorkspaceState opens a diff preview in a new pane to the right", () => {
    const next = openDiffPreviewInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
      {
        changeType: "edit",
        diff: "-before\n+after",
        diffMessageId: "diff-a",
        filePath: "/tmp/app.ts",
        language: "typescript",
        originSessionId: "session-a",
        summary: "Updated app.ts",
      },
      "pane-a",
    );

    expect(next.panes).toHaveLength(2);
    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      tabs: [makeSessionTab("tab-a", "session-a")],
      activeTabId: "tab-a",
      viewMode: "session",
    });
    expect(next.panes.find((pane) => pane.id !== "pane-a")).toMatchObject({
      tabs: [
        {
          id: expect.any(String),
          kind: "diffPreview",
          diffMessageId: "diff-a",
          filePath: "/tmp/app.ts",
          originSessionId: "session-a",
        },
      ],
      viewMode: "diffPreview",
      activeSessionId: "session-a",
    });
  });
  it("openDiffPreviewInWorkspaceState opens a control-panel diff beside the session pane", () => {
    const next = openDiffPreviewInWorkspaceState(
      makeSplitWorkspace(
        makePane("pane-a", [makeControlPanelTab("control-a", null)], {
          activeTabId: "control-a",
          activeSessionId: null,
          viewMode: "controlPanel",
        }),
        makePane("pane-b", [makeSessionTab("tab-b", "session-b")]),
        "pane-a",
      ),
      {
        changeType: "edit",
        diff: "-before\n+after",
        diffMessageId: "diff-a",
        filePath: "/tmp/app.ts",
        language: "typescript",
        originSessionId: null,
        summary: "Updated app.ts",
      },
      "pane-a",
      {
        reuseActiveViewerTab: true,
      },
    );
    const diffPane = next.panes.find((pane) =>
      pane.tabs.some((tab) => tab.kind === "diffPreview"),
    );

    expect(next.panes).toHaveLength(3);
    expect(diffPane).toBeTruthy();
    expect(next.activePaneId).toBe(diffPane?.id ?? null);
    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      tabs: [makeControlPanelTab("control-a", null)],
      activeTabId: "control-a",
      viewMode: "controlPanel",
      activeSessionId: null,
    });
    expect(next.panes.find((pane) => pane.id === "pane-b")).toMatchObject({
      tabs: [makeSessionTab("tab-b", "session-b")],
      activeTabId: "tab-b",
      viewMode: "session",
      activeSessionId: "session-b",
    });
    expect(diffPane).toMatchObject({
      tabs: [
        {
          id: expect.any(String),
          kind: "diffPreview",
          changeType: "edit",
          diff: "-before\n+after",
          diffMessageId: "diff-a",
          filePath: "/tmp/app.ts",
          language: "typescript",
          originSessionId: null,
          summary: "Updated app.ts",
        },
      ],
      viewMode: "diffPreview",
    });
  });

  it("openDiffPreviewInWorkspaceState focuses an existing diff tab with the same change set", () => {
    const next = openDiffPreviewInWorkspaceState(
      makeSplitWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
        makePane(
          "pane-b",
          [
            makeDiffPreviewTab(
              "diff-tab-a",
              "diff-a",
              "/tmp/app.ts",
              "session-a",
              "change-shared",
            ),
          ],
          {
            activeTabId: "diff-tab-a",
            activeSessionId: "session-a",
            viewMode: "diffPreview",
          },
        ),
      ),
      {
        changeType: "edit",
        changeSetId: "change-shared",
        diff: "-foo\n+bar",
        diffMessageId: "diff-b",
        filePath: "/tmp/next.ts",
        language: "typescript",
        originSessionId: "session-a",
        summary: "Updated next.ts",
      },
      "pane-a",
    );

    expect(next.activePaneId).toBe("pane-b");
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      makeDiffPreviewTab(
        "diff-tab-a",
        "diff-a",
        "/tmp/app.ts",
        "session-a",
        "change-shared",
      ),
    ]);
  });

  it("openDiffPreviewInWorkspaceState opens a new tab instead of reusing an existing viewer when requested", () => {
    const next = openDiffPreviewInWorkspaceState(
      makeSplitWorkspace(
        makePane("pane-a", [makeGitStatusTab("git-a", "/tmp/project", null)], {
          activeTabId: "git-a",
          activeSessionId: null,
          viewMode: "gitStatus",
        }),
        makePane(
          "pane-b",
          [makeDiffPreviewTab("diff-tab-a", "diff-a", "/tmp/app.ts", null)],
          {
            activeTabId: "diff-tab-a",
            activeSessionId: null,
            viewMode: "diffPreview",
          },
        ),
      ),
      {
        changeType: "edit",
        diff: "-foo\n+bar",
        diffMessageId: "diff-b",
        filePath: "/tmp/next.ts",
        gitSectionId: "staged",
        language: "typescript",
        originSessionId: null,
        summary: "Updated next.ts",
      },
      "pane-a",
      {
        openInNewTab: true,
        reuseActiveViewerTab: true,
      },
    );

    expect(next.panes).toHaveLength(2);
    expect(next.activePaneId).toBe("pane-b");
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      makeDiffPreviewTab("diff-tab-a", "diff-a", "/tmp/app.ts", null),
      {
        id: expect.any(String),
        kind: "diffPreview",
        changeType: "edit",
        diff: "-foo\n+bar",
        diffMessageId: "diff-b",
        filePath: "/tmp/next.ts",
        gitSectionId: "staged",
        language: "typescript",
        originSessionId: null,
        summary: "Updated next.ts",
      },
    ]);
    expect(next.panes.find((pane) => pane.id === "pane-b")).toMatchObject({
      viewMode: "diffPreview",
      activeSessionId: null,
    });
  });

  it("openDiffPreviewInWorkspaceState reuses the existing diff pane for later previews", () => {
    const next = openDiffPreviewInWorkspaceState(
      makeSplitWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
        makePane(
          "pane-b",
          [
            makeDiffPreviewTab(
              "diff-tab-a",
              "diff-a",
              "/tmp/app.ts",
              "session-a",
            ),
          ],
          {
            activeTabId: "diff-tab-a",
            activeSessionId: "session-a",
            viewMode: "diffPreview",
          },
        ),
      ),
      {
        changeType: "edit",
        diff: "-foo\n+bar",
        diffMessageId: "diff-b",
        filePath: "/tmp/next.ts",
        language: "typescript",
        originSessionId: "session-a",
        summary: "Updated next.ts",
      },
      "pane-a",
    );

    expect(next.activePaneId).toBe("pane-b");
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      makeDiffPreviewTab("diff-tab-a", "diff-a", "/tmp/app.ts", "session-a"),
      {
        id: expect.any(String),
        kind: "diffPreview",
        changeType: "edit",
        diff: "-foo\n+bar",
        diffMessageId: "diff-b",
        filePath: "/tmp/next.ts",
        language: "typescript",
        originSessionId: "session-a",
        summary: "Updated next.ts",
      },
    ]);
  });

  it("openDiffPreviewInWorkspaceState replaces the active diff tab when opened from git status", () => {
    const next = openDiffPreviewInWorkspaceState(
      makeSplitWorkspace(
        makePane("pane-a", [makeGitStatusTab("git-a", "/tmp/project", null)], {
          activeTabId: "git-a",
          activeSessionId: null,
          viewMode: "gitStatus",
        }),
        makePane(
          "pane-b",
          [makeDiffPreviewTab("diff-tab-a", "diff-a", "/tmp/app.ts", null)],
          {
            activeTabId: "diff-tab-a",
            activeSessionId: null,
            viewMode: "diffPreview",
          },
        ),
      ),
      {
        changeType: "edit",
        diff: "-foo\n+bar",
        diffMessageId: "diff-b",
        filePath: "/tmp/next.ts",
        gitSectionId: "staged",
        language: "typescript",
        originSessionId: null,
        summary: "Updated next.ts",
      },
      "pane-a",
      {
        reuseActiveViewerTab: true,
      },
    );

    expect(next.panes).toHaveLength(2);
    expect(next.activePaneId).toBe("pane-b");
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      {
        id: expect.any(String),
        kind: "diffPreview",
        changeType: "edit",
        diff: "-foo\n+bar",
        diffMessageId: "diff-b",
        filePath: "/tmp/next.ts",
        gitSectionId: "staged",
        language: "typescript",
        originSessionId: null,
        summary: "Updated next.ts",
      },
    ]);
  });

  it("openDiffPreviewInWorkspaceState replaces the active source tab when opened from git status", () => {
    const next = openDiffPreviewInWorkspaceState(
      makeSplitWorkspace(
        makePane("pane-a", [makeGitStatusTab("git-a", "/tmp/project", null)], {
          activeTabId: "git-a",
          activeSessionId: null,
          viewMode: "gitStatus",
        }),
        makePane("pane-b", [makeSourceTab("source-a", "/tmp/app.ts", null)], {
          activeTabId: "source-a",
          activeSessionId: null,
          viewMode: "source",
          sourcePath: "/tmp/app.ts",
        }),
      ),
      {
        changeType: "edit",
        diff: "-foo\n+bar",
        diffMessageId: "diff-b",
        filePath: "/tmp/next.ts",
        language: "typescript",
        originSessionId: null,
        summary: "Updated next.ts",
      },
      "pane-a",
      {
        reuseActiveViewerTab: true,
      },
    );

    expect(next.panes).toHaveLength(2);
    expect(next.activePaneId).toBe("pane-b");
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      {
        id: expect.any(String),
        kind: "diffPreview",
        changeType: "edit",
        diff: "-foo\n+bar",
        diffMessageId: "diff-b",
        filePath: "/tmp/next.ts",
        language: "typescript",
        originSessionId: null,
        summary: "Updated next.ts",
      },
    ]);
  });

  it("openDiffPreviewInWorkspaceState prefers the nearest viewer pane for standalone git tabs", () => {
    const leftViewerPane = makePane(
      "pane-a",
      [makeSourceTab("source-left", "/tmp/left.ts", null)],
      {
        activeTabId: "source-left",
        activeSessionId: null,
        viewMode: "source",
        sourcePath: "/tmp/left.ts",
      },
    );
    const standaloneSessionsPane = makePane(
      "pane-b",
      [makeSessionListTab("sessions-b", null)],
      {
        activeTabId: "sessions-b",
        activeSessionId: null,
        viewMode: "controlPanel",
      },
    );
    const standaloneGitPane = makePane(
      "pane-c",
      [makeGitStatusTab("git-c", "/tmp/project", null)],
      {
        activeTabId: "git-c",
        activeSessionId: null,
        viewMode: "gitStatus",
      },
    );
    const rightViewerPane = makePane(
      "pane-d",
      [makeSourceTab("source-right", "/tmp/right.ts", null)],
      {
        activeTabId: "source-right",
        activeSessionId: null,
        viewMode: "source",
        sourcePath: "/tmp/right.ts",
      },
    );
    const workspace: WorkspaceState = {
      root: {
        id: "split-root",
        type: "split",
        direction: "row",
        ratio: 0.5,
        first: {
          id: "split-left",
          type: "split",
          direction: "row",
          ratio: 0.5,
          first: { type: "pane", paneId: leftViewerPane.id },
          second: { type: "pane", paneId: standaloneSessionsPane.id },
        },
        second: {
          id: "split-right",
          type: "split",
          direction: "row",
          ratio: 0.5,
          first: { type: "pane", paneId: standaloneGitPane.id },
          second: { type: "pane", paneId: rightViewerPane.id },
        },
      },
      panes: [
        leftViewerPane,
        standaloneSessionsPane,
        standaloneGitPane,
        rightViewerPane,
      ],
      activePaneId: standaloneGitPane.id,
    };

    const next = openDiffPreviewInWorkspaceState(
      workspace,
      {
        changeType: "edit",
        diff: "-foo\n+bar",
        diffMessageId: "diff-nearest",
        filePath: "/tmp/next.ts",
        gitSectionId: "staged",
        language: "typescript",
        originSessionId: null,
        summary: "Updated next.ts",
      },
      standaloneGitPane.id,
      {
        reuseActiveViewerTab: true,
      },
    );

    expect(next.activePaneId).toBe(rightViewerPane.id);
    expect(
      next.panes.find((pane) => pane.id === rightViewerPane.id)?.tabs,
    ).toEqual([
      {
        id: expect.any(String),
        kind: "diffPreview",
        changeType: "edit",
        diff: "-foo\n+bar",
        diffMessageId: "diff-nearest",
        filePath: "/tmp/next.ts",
        gitSectionId: "staged",
        language: "typescript",
        originSessionId: null,
        summary: "Updated next.ts",
      },
    ]);
    expect(
      next.panes.find((pane) => pane.id === leftViewerPane.id)?.tabs,
    ).toEqual([makeSourceTab("source-left", "/tmp/left.ts", null)]);
    expect(
      next.panes.find((pane) => pane.id === standaloneSessionsPane.id)?.tabs,
    ).toEqual([makeSessionListTab("sessions-b", null)]);
  });

  it("openDiffPreviewInWorkspaceState keeps docked git diffs local when a far viewer is active", () => {
    const controlPane = makePane(
      "pane-a",
      [makeControlPanelTab("control-a", null)],
      {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      },
    );
    const localSessionPane = makePane(
      "pane-b",
      [makeSessionTab("session-b", "session-b")],
      {
        activeTabId: "session-b",
        activeSessionId: "session-b",
        viewMode: "session",
      },
    );
    const middleSessionPane = makePane(
      "pane-c",
      [makeSessionTab("session-c", "session-c")],
      {
        activeTabId: "session-c",
        activeSessionId: "session-c",
        viewMode: "session",
      },
    );
    const farViewerPane = makePane(
      "pane-d",
      [makeDiffPreviewTab("diff-d", "diff-existing", "/tmp/existing.ts", null)],
      {
        activeTabId: "diff-d",
        activeSessionId: null,
        viewMode: "diffPreview",
      },
    );
    const workspace: WorkspaceState = {
      root: {
        id: "split-root",
        type: "split",
        direction: "row",
        ratio: 0.25,
        first: { type: "pane", paneId: controlPane.id },
        second: {
          id: "split-rest",
          type: "split",
          direction: "row",
          ratio: 0.34,
          first: { type: "pane", paneId: localSessionPane.id },
          second: {
            id: "split-tail",
            type: "split",
            direction: "row",
            ratio: 0.5,
            first: { type: "pane", paneId: middleSessionPane.id },
            second: { type: "pane", paneId: farViewerPane.id },
          },
        },
      },
      panes: [controlPane, localSessionPane, middleSessionPane, farViewerPane],
      activePaneId: farViewerPane.id,
    };

    const next = openDiffPreviewInWorkspaceState(
      workspace,
      {
        changeType: "edit",
        diff: "-foo\n+bar",
        diffMessageId: "diff-local",
        filePath: "/tmp/local.ts",
        gitSectionId: "staged",
        language: "typescript",
        originSessionId: null,
        summary: "Updated local.ts",
      },
      controlPane.id,
      {
        reuseActiveViewerTab: true,
      },
    );

    expect(next.activePaneId).toBe(localSessionPane.id);
    expect(
      next.panes.find((pane) => pane.id === localSessionPane.id)?.tabs,
    ).toEqual([
      makeSessionTab("session-b", "session-b"),
      {
        id: expect.any(String),
        kind: "diffPreview",
        changeType: "edit",
        diff: "-foo\n+bar",
        diffMessageId: "diff-local",
        filePath: "/tmp/local.ts",
        gitSectionId: "staged",
        language: "typescript",
        originSessionId: null,
        summary: "Updated local.ts",
      },
    ]);
    expect(
      next.panes.find((pane) => pane.id === farViewerPane.id)?.tabs,
    ).toEqual([
      makeDiffPreviewTab("diff-d", "diff-existing", "/tmp/existing.ts", null),
    ]);
  });

  it("updateGitDiffPreviewTabInWorkspaceState hydrates a pending git diff tab in place", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane(
        "pane-a",
        [
          {
            id: "diff-pending-tab",
            kind: "diffPreview",
            changeType: "edit",
            diff: "",
            diffMessageId: "git-preview:pane-a:/repo:staged::src/main.rs",
            filePath: "src/main.rs",
            gitSectionId: "staged",
            originSessionId: null,
            summary: "Loading staged changes in src/main.rs",
            gitDiffRequestKey: "git-preview:pane-a:/repo:staged::src/main.rs",
            isLoading: true,
          },
        ],
        {
          activeTabId: "diff-pending-tab",
          activeSessionId: null,
          viewMode: "diffPreview",
        },
      ),
    );

    const next = updateGitDiffPreviewTabInWorkspaceState(
      workspace,
      "git-preview:pane-a:/repo:staged::src/main.rs",
      (tab) => ({
        ...tab,
        changeSetId: "git-diff-123",
        diff: "@@ -1 +1 @@\n-old\n+new",
        filePath: "/repo/src/main.rs",
        language: "rust",
        summary: "Staged changes in src/main.rs",
        isLoading: false,
        loadError: null,
      }),
    );

    expect(next.panes[0]?.tabs).toEqual([
      {
        id: "diff-pending-tab",
        kind: "diffPreview",
        changeType: "edit",
        changeSetId: "git-diff-123",
        diff: "@@ -1 +1 @@\n-old\n+new",
        diffMessageId: "git-preview:pane-a:/repo:staged::src/main.rs",
        filePath: "/repo/src/main.rs",
        gitSectionId: "staged",
        language: "rust",
        originSessionId: null,
        summary: "Staged changes in src/main.rs",
        gitDiffRequestKey: "git-preview:pane-a:/repo:staged::src/main.rs",
        isLoading: false,
        loadError: null,
      },
    ]);
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

  it("upsertCanvasSessionCard adds and moves canvas cards without duplicates", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane(
        "pane-a",
        [
          makeCanvasTab(
            "canvas-a",
            [{ sessionId: "session-a", x: 80, y: 90 }],
            null,
          ),
        ],
        {
          activeTabId: "canvas-a",
          activeSessionId: null,
          viewMode: "canvas",
        },
      ),
    );

    const withNewCard = upsertCanvasSessionCard(workspace, "canvas-a", {
      sessionId: "session-b",
      x: 240.2,
      y: 360.7,
    });
    expect(withNewCard.panes[0]?.tabs[0]).toEqual(
      makeCanvasTab(
        "canvas-a",
        [
          { sessionId: "session-a", x: 80, y: 90 },
          { sessionId: "session-b", x: 240, y: 361 },
        ],
        null,
      ),
    );

    const moved = upsertCanvasSessionCard(withNewCard, "canvas-a", {
      sessionId: "session-a",
      x: 400,
      y: 420,
    });
    expect(moved.panes[0]?.tabs[0]).toEqual(
      makeCanvasTab(
        "canvas-a",
        [
          { sessionId: "session-a", x: 400, y: 420 },
          { sessionId: "session-b", x: 240, y: 361 },
        ],
        null,
      ),
    );

    const removed = removeCanvasSessionCard(moved, "canvas-a", "session-b");
    expect(removed.panes[0]?.tabs[0]).toEqual(
      makeCanvasTab(
        "canvas-a",
        [{ sessionId: "session-a", x: 400, y: 420 }],
        null,
      ),
    );
  });

  it("setCanvasZoom stores normalized zoom and omits the default value", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane(
        "pane-a",
        [
          makeCanvasTab(
            "canvas-a",
            [{ sessionId: "session-a", x: 80, y: 90 }],
            null,
          ),
        ],
        {
          activeTabId: "canvas-a",
          activeSessionId: null,
          viewMode: "canvas",
        },
      ),
    );

    const zoomed = setCanvasZoom(workspace, "canvas-a", 1.2376);
    expect(zoomed.panes[0]?.tabs[0]).toEqual(
      makeCanvasTab(
        "canvas-a",
        [{ sessionId: "session-a", x: 80, y: 90 }],
        null,
        null,
        1.238,
      ),
    );

    const reset = setCanvasZoom(zoomed, "canvas-a", 1);
    expect(reset.panes[0]?.tabs[0]).toEqual(
      makeCanvasTab(
        "canvas-a",
        [{ sessionId: "session-a", x: 80, y: 90 }],
        null,
      ),
    );
  });

  it("setPaneSourcePath focuses an existing source tab for the same path instead of duplicating it", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane(
        "pane-a",
        [
          makeSourceTab("source-a", "/tmp/app.ts", "session-a"),
          makeSourceTab("source-b", null, "session-a"),
        ],
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
        makePane(
          "pane-a",
          [
            makeSessionTab("tab-a", "session-a"),
            makeSessionTab("tab-b", "session-b"),
          ],
          {
            activeTabId: "tab-b",
            activeSessionId: "session-b",
          },
        ),
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
        makePane("pane-b", [
          makeSessionTab("tab-b", "session-b"),
          makeSessionTab("tab-c", "session-c"),
        ]),
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
      tabs: [
        makeSessionTab("tab-b", "session-b"),
        makeSessionTab("tab-a", "session-a"),
        makeSessionTab("tab-c", "session-c"),
      ],
      activeSessionId: "session-a",
    });
  });

  it("placeDraggedTab rejects vertical control panel placement", () => {
    const workspace = makeSplitWorkspace(
      makePane("pane-a", [makeControlPanelTab("control-a", null)], {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      }),
      makePane("pane-b", [makeSessionTab("tab-b", "session-b")]),
    );

    const next = placeDraggedTab(
      workspace,
      "pane-a",
      "control-a",
      "pane-b",
      "top",
    );

    expect(next).toEqual(workspace);
  });

  it("placeDraggedTab rejects tab-stacking into the control panel pane", () => {
    const workspace = makeSplitWorkspace(
      makePane("pane-a", [makeControlPanelTab("control-a", null)], {
        activeTabId: "control-a",
        activeSessionId: null,
        viewMode: "controlPanel",
      }),
      makePane("pane-b", [makeSessionTab("tab-b", "session-b")]),
    );

    const next = placeDraggedTab(
      workspace,
      "pane-b",
      "tab-b",
      "pane-a",
      "tabs",
      1,
    );

    expect(next).toEqual(workspace);
  });

  it("placeExternalTab clones a dropped tab into the target pane", () => {
    const externalTab = makeSourceTab(
      "source-external",
      "/tmp/external.ts",
      "session-a",
    );
    const next = placeExternalTab(
      makeSplitWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
        makePane("pane-b", [makeSessionTab("tab-b", "session-b")]),
      ),
      externalTab,
      "pane-b",
      "tabs",
      0,
    );

    const targetPane = next.panes.find((pane) => pane.id === "pane-b");
    const insertedTab = targetPane?.tabs[0];

    expect(next.activePaneId).toBe("pane-b");
    expect(insertedTab).toMatchObject({
      kind: "source",
      path: "/tmp/external.ts",
      originSessionId: "session-a",
    });
    expect(insertedTab?.id).not.toBe("source-external");
    expect(targetPane?.activeTabId).toBe(insertedTab?.id ?? null);
  });

  it("placeExternalTab creates an adjacent pane for side drops", () => {
    const externalTab = makeSessionTab("tab-external", "session-c");
    const next = placeExternalTab(
      makeSplitWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
        makePane("pane-b", [makeSessionTab("tab-b", "session-b")]),
      ),
      externalTab,
      "pane-b",
      "left",
    );

    const importedPane = next.panes.find(
      (pane) => pane.id !== "pane-a" && pane.id !== "pane-b",
    );

    expect(next.panes).toHaveLength(3);
    expect(next.activePaneId).toBe(importedPane?.id ?? null);
    expect(importedPane).toMatchObject({
      tabs: [
        {
          kind: "session",
          sessionId: "session-c",
        },
      ],
      activeSessionId: "session-c",
    });
    expect(importedPane?.tabs[0]?.id).not.toBe("tab-external");
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
          [
            makeSessionTab("tab-a", "session-a"),
            makeSessionTab("tab-b", "session-b"),
            makeSourceTab("source-a", "/tmp/a.ts", "session-b"),
          ],
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

  it("reconcileWorkspaceState prunes missing canvas cards and normalizes canvas origin metadata", () => {
    const next = reconcileWorkspaceState(
      makeSinglePaneWorkspace(
        makePane(
          "pane-a",
          [
            makeCanvasTab(
              "canvas-a",
              [
                { sessionId: "session-a", x: 120.4, y: 200.2 },
                { sessionId: "session-missing", x: 480, y: 520 },
              ],
              "session-a",
              "  project-a  ",
            ),
          ],
          {
            activeTabId: "canvas-a",
            activeSessionId: null,
            viewMode: "canvas",
          },
        ),
      ),
      [
        {
          ...makeSession("session-a"),
          projectId: "project-a",
        },
      ],
    );

    expect(next.panes[0]).toMatchObject({
      activeTabId: "canvas-a",
      activeSessionId: "session-a",
      viewMode: "canvas",
    });
    expect(next.panes[0]?.tabs[0]).toEqual({
      id: "canvas-a",
      kind: "canvas",
      cards: [{ sessionId: "session-a", x: 120, y: 200 }],
      originSessionId: "session-a",
      originProjectId: "project-a",
    });
  });

  it("reconcileWorkspaceState updates origin fields for session and project list tabs", () => {
    const next = reconcileWorkspaceState(
      makeSinglePaneWorkspace(
        makePane(
          "pane-a",
          [
            makeSessionListTab("sessions-a", "session-a", "  project-a  "),
            makeProjectListTab("projects-a", "session-b", "  project-b  "),
          ],
          {
            activeTabId: "projects-a",
            activeSessionId: "session-a",
            viewMode: "projectList",
          },
        ),
      ),
      [
        {
          ...makeSession("session-b"),
          projectId: "project-b",
        },
      ],
    );

    expect(next.panes[0].tabs).toEqual([
      {
        id: "sessions-a",
        kind: "sessionList",
        originSessionId: null,
        originProjectId: "project-a",
      },
      {
        id: "projects-a",
        kind: "projectList",
        originSessionId: "session-b",
        originProjectId: "project-b",
      },
    ]);
    expect(next.panes[0]).toMatchObject({
      activeTabId: "projects-a",
      activeSessionId: "session-b",
      viewMode: "projectList",
    });
  });

  it("dockControlPanelAtWorkspaceEdge uses a preferred control panel width ratio when provided", () => {
    const workspace = {
      root: {
        id: "split-1",
        type: "split" as const,
        direction: "row" as const,
        ratio: 0.5,
        first: {
          type: "pane" as const,
          paneId: "pane-control",
        },
        second: {
          type: "pane" as const,
          paneId: "pane-session",
        },
      },
      panes: [
        makePane("pane-control", [makeControlPanelTab("control-a", null)], {
          activeTabId: "control-a",
          activeSessionId: null,
          viewMode: "controlPanel",
        }),
        makePane("pane-session", [makeSessionTab("tab-a", "session-a")], {
          activeTabId: "tab-a",
          activeSessionId: "session-a",
          viewMode: "session",
        }),
      ],
      activePaneId: "pane-session",
    };

    const next = dockControlPanelAtWorkspaceEdge(workspace, "left", 0.31);

    expect(next.root).toMatchObject({
      type: "split",
      direction: "row",
      ratio: 0.31,
    });
  });
});
