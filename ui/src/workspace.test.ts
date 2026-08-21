import type { Session } from "./types";
import {
  addWorkspaceTabToPane,
  activatePane,
  closeWorkspaceTab,
  collectWorkspaceSessionReferences,
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
  openTerminalInWorkspaceState,
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
  resolveWorkspaceViewerSplitAnchorPaneId,
  setCanvasZoom,
  setPaneSourcePath,
  splitPane,
  stripLoadingGitDiffPreviewTabsFromWorkspaceState,
  updateGitDiffPreviewTabInWorkspaceState,
  updateSplitRatio,
  upsertCanvasSessionCard,
  workspaceHasDelegatedChildSessionReferences,
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

function makeTerminalTab(
  id: string,
  workdir: string | null,
  originSessionId: string | null,
  originProjectId: string | null = null,
): WorkspaceTab {
  return {
    id,
    kind: "terminal",
    workdir,
    originSessionId,
    ...(originProjectId ? { originProjectId } : {}),
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

function makeResponseBoardTab(
  id: string,
  originSessionId: string | null,
): WorkspaceTab {
  return {
    id,
    kind: "responseBoard",
    originSessionId,
    refreshToken: `${id}-refresh`,
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
    tabVisitHistory?: string[];
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
    ...(options?.tabVisitHistory
      ? { tabVisitHistory: options.tabVisitHistory }
      : {}),
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

  it("activatePane remembers the last content pane while focus moves to a control surface", () => {
    const contentPane = makePane(
      "pane-content",
      [makeSessionTab("tab-session", "session-a")],
      { activeTabId: "tab-session" },
    );
    const controlPane = makePane(
      "pane-control",
      [makeControlPanelTab("tab-control", "session-a")],
      {
        activeTabId: "tab-control",
        activeSessionId: null,
        viewMode: "controlPanel",
      },
    );
    const workspace = makeSplitWorkspace(
      contentPane,
      controlPane,
      contentPane.id,
    );

    const focusedControl = activatePane(workspace, controlPane.id);
    const opened = openSessionInWorkspaceState(
      focusedControl,
      "session-b",
      controlPane.id,
    );

    expect(focusedControl.lastContentPaneId).toBe(contentPane.id);
    expect(opened.activePaneId).toBe(contentPane.id);
    expect(
      opened.panes.find((pane) => pane.id === contentPane.id)?.tabs,
    ).toEqual([
      makeSessionTab("tab-session", "session-a"),
      expect.objectContaining({ kind: "session", sessionId: "session-b" }),
    ]);
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

  it("openCanvasInWorkspaceState refreshes origin metadata when reusing an existing canvas tab", () => {
    const next = openCanvasInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane(
          "pane-a",
          [
            makeSessionTab("tab-a", "session-a"),
            makeCanvasTab("canvas-a", [], null),
          ],
          {
            activeTabId: "tab-a",
            activeSessionId: "session-a",
            viewMode: "session",
          },
        ),
      ),
      "pane-a",
      "session-a",
      "project-a",
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes[0]).toMatchObject({
      activeTabId: "canvas-a",
      activeSessionId: "session-a",
      viewMode: "canvas",
    });
    expect(next.panes[0].tabs[1]).toEqual({
      id: "canvas-a",
      kind: "canvas",
      cards: [],
      originSessionId: "session-a",
      originProjectId: "project-a",
    });
  });

  it("openCanvasInWorkspaceState activates an existing canvas in place", () => {
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

    expect(next.activePaneId).toBe(remoteCanvasPane.id);
    expect(next.panes.find((pane) => pane.id === controlPane.id)?.tabs).toEqual(
      [makeControlPanelTab("control-a", null)],
    );
    expect(
      next.panes.find((pane) => pane.id === targetSessionPane.id)?.tabs,
    ).toEqual([makeSessionTab("tab-target", "session-target")]);
    expect(
      next.panes.find((pane) => pane.id === remoteCanvasPane.id)?.tabs,
    ).toEqual([
      makeSessionTab("tab-review", "session-review"),
      {
        id: "canvas-a",
        kind: "canvas",
        cards: [],
        originSessionId: "session-target",
        originProjectId: "project-target",
      },
    ]);
    expect(
      next.panes.find((pane) => pane.id === remoteCanvasPane.id)?.activeTabId,
    ).toBe("canvas-a");
    expect(
      next.panes.find((pane) => pane.id === remoteCanvasPane.id)
        ?.activeSessionId,
    ).toBe("session-target");
    expect(
      next.panes.find((pane) => pane.id === remoteCanvasPane.id)?.viewMode,
    ).toBe("canvas");
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

  it("openOrchestratorListInWorkspaceState updates and activates the library tab in place", () => {
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

    expect(next.activePaneId).toBe(sessionPaneA.id);
    expect(
      next.panes.find((pane) => pane.id === sessionPaneA.id)?.tabs,
    ).toEqual([
      makeSessionTab("tab-a", "session-a"),
      {
        ...existingOrchestratorTab,
        originSessionId: "session-b",
        originProjectId: "project-b",
      },
    ]);
    expect(
      next.panes.find((pane) => pane.id === sessionPaneB.id)?.tabs,
    ).toEqual([makeSessionTab("tab-b", "session-b")]);
    expect(
      next.panes.find((pane) => pane.id === sessionPaneA.id)?.activeTabId,
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

  it("openSessionInWorkspaceState activates an existing session without moving it", () => {
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

    expect(next.activePaneId).toBe(remoteSessionPane.id);
    expect(next.panes.find((pane) => pane.id === controlPane.id)?.tabs).toEqual(
      [makeControlPanelTab("control-a", null)],
    );
    expect(
      next.panes.find((pane) => pane.id === targetSessionPane.id)?.tabs,
    ).toEqual([makeSessionTab("tab-target", "session-target")]);
    expect(
      next.panes.find((pane) => pane.id === remoteSessionPane.id)?.activeTabId,
    ).toBe("tab-review");
    expect(
      next.panes.find((pane) => pane.id === remoteSessionPane.id)
        ?.activeSessionId,
    ).toBe("session-review");
    expect(next.panes).toHaveLength(3);
  });

  it("openSessionInWorkspaceState prefers the last focused content pane", () => {
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
      lastContentPaneId: rightSessionPane.id,
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

  it("openSessionInWorkspaceState reuses the last content pane without an implicit split", () => {
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

    expect(next.panes).toHaveLength(2);
    expect(next.activePaneId).toBe(sessionPane.id);
    expect(next.panes.find((pane) => pane.id === controlPane.id)?.tabs).toEqual(
      [makeControlPanelTab("control-a", null)],
    );
    expect(next.panes.find((pane) => pane.id === sessionPane.id)).toMatchObject(
      {
        tabs: [
          makeSessionTab("tab-session", "session-a"),
          {
            id: expect.any(String),
            kind: "session",
            sessionId: "session-b",
          },
        ],
        activeSessionId: "session-b",
        viewMode: "session",
      },
    );
  });

  it("openSessionInWorkspaceState keeps new sessions in the primary pane while the viewer is active", () => {
    const sessionPane = makePane(
      "pane-session",
      [makeSessionTab("tab-session", "session-a")],
      {
        activeTabId: "tab-session",
        activeSessionId: "session-a",
        viewMode: "session",
      },
    );
    const viewerPane = makePane(
      "pane-viewer",
      [makeSourceTab("source-a", "/tmp/app.ts", "session-a")],
      {
        activeTabId: "source-a",
        activeSessionId: "session-a",
        viewMode: "source",
        sourcePath: "/tmp/app.ts",
      },
    );
    const workspace: WorkspaceState = {
      ...makeSplitWorkspace(sessionPane, viewerPane, viewerPane.id),
      lastContentPaneId: sessionPane.id,
      lastViewerPaneId: viewerPane.id,
    };

    const next = openSessionInWorkspaceState(
      workspace,
      "session-b",
      viewerPane.id,
    );

    expect(next.panes).toHaveLength(2);
    expect(next.activePaneId).toBe(sessionPane.id);
    expect(next.lastContentPaneId).toBe(sessionPane.id);
    expect(next.lastViewerPaneId).toBe(viewerPane.id);
    expect(next.panes.find((pane) => pane.id === sessionPane.id)?.tabs).toEqual(
      [
        makeSessionTab("tab-session", "session-a"),
        {
          id: expect.any(String),
          kind: "session",
          sessionId: "session-b",
        },
      ],
    );
    expect(next.panes.find((pane) => pane.id === viewerPane.id)?.tabs).toEqual([
      makeSourceTab("source-a", "/tmp/app.ts", "session-a"),
    ]);
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

  it("openInstructionDebuggerInWorkspaceState focuses a restored debugger tab with a legacy Windows verbatim workdir", () => {
    const legacyWorkdir = String.raw`\\?\C:\repo`;
    const normalizedWorkdir = String.raw`C:\repo`;
    const paneA = makePane("pane-a", [
      makeSessionTab("tab-session", "session-a"),
      makeInstructionDebuggerTab(
        "tab-instructions",
        legacyWorkdir,
        "session-a",
      ),
    ]);
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openInstructionDebuggerInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      normalizedWorkdir,
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

  it("control-panel layout normalization preserves existing pane and tab ownership", () => {
    const paneA = makePane("pane-a", [makeSessionTab("tab-a", "session-a")]);
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);
    const workspace = makeSplitWorkspace(paneA, paneB, paneA.id);
    const existingTabOwners = new Map(
      workspace.panes.flatMap((pane) =>
        pane.tabs.map((tab): [string, string] => [tab.id, pane.id]),
      ),
    );

    const ensured = ensureControlPanelInWorkspaceState(workspace);
    const docked = dockControlPanelAtWorkspaceEdge(ensured, "right");

    expect(workspace.panes.map((pane) => pane.id).every((paneId) =>
      docked.panes.some((pane) => pane.id === paneId),
    )).toBe(true);
    for (const [tabId, paneId] of existingTabOwners) {
      expect(
        docked.panes.find((pane) =>
          pane.tabs.some((tab) => tab.id === tabId),
        )?.id,
      ).toBe(paneId);
    }
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

  it("closeWorkspaceTab returns to the most recently visited surviving tab", () => {
    const initial = makeSinglePaneWorkspace(
      makePane(
        "pane-a",
        [
          makeSessionTab("tab-a", "session-a"),
          makeSessionTab("tab-b", "session-b"),
          makeSessionTab("tab-c", "session-c"),
          makeSessionTab("tab-d", "session-d"),
        ],
        {
          activeTabId: "tab-a",
          activeSessionId: "session-a",
        },
      ),
    );

    const visitedC = activatePane(initial, "pane-a", "tab-c");
    const visitedD = activatePane(visitedC, "pane-a", "tab-d");
    const closedD = closeWorkspaceTab(visitedD, "pane-a", "tab-d");
    const closedC = closeWorkspaceTab(closedD, "pane-a", "tab-c");

    expect(closedD.panes[0].activeTabId).toBe("tab-c");
    expect(closedD.panes[0].tabVisitHistory).toEqual(["tab-c", "tab-a"]);
    expect(closedC.panes[0].activeTabId).toBe("tab-a");
    expect(closedC.panes[0].activeSessionId).toBe("session-a");
    expect(closedC.panes[0].tabVisitHistory).toEqual(["tab-a"]);
  });

  it("closeWorkspaceTab prunes inactive and stale history entries", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane(
        "pane-a",
        [
          makeSessionTab("tab-a", "session-a"),
          makeSessionTab("tab-b", "session-b"),
          makeSessionTab("tab-c", "session-c"),
        ],
        {
          activeTabId: "tab-c",
          activeSessionId: "session-c",
          tabVisitHistory: ["tab-c", "tab-a", "tab-stale", "tab-b"],
        },
      ),
    );

    const next = closeWorkspaceTab(workspace, "pane-a", "tab-a");

    expect(next.panes[0].activeTabId).toBe("tab-c");
    expect(next.panes[0].tabVisitHistory).toEqual(["tab-c", "tab-b"]);
  });

  it("keeps tab visit history isolated per pane", () => {
    const paneA = makePane(
      "pane-a",
      [
        makeSessionTab("tab-a1", "session-a1"),
        makeSessionTab("tab-a2", "session-a2"),
        makeSessionTab("tab-a3", "session-a3"),
      ],
      { activeTabId: "tab-a1", activeSessionId: "session-a1" },
    );
    const paneB = makePane(
      "pane-b",
      [
        makeSessionTab("tab-b1", "session-b1"),
        makeSessionTab("tab-b2", "session-b2"),
      ],
      { activeTabId: "tab-b1", activeSessionId: "session-b1" },
    );
    const workspace: WorkspaceState = {
      root: {
        id: "split-root",
        type: "split",
        direction: "row",
        ratio: 0.5,
        first: { type: "pane", paneId: "pane-a" },
        second: { type: "pane", paneId: "pane-b" },
      },
      panes: [paneA, paneB],
      activePaneId: "pane-a",
    };

    const visitedA3 = activatePane(workspace, "pane-a", "tab-a3");
    const visitedB2 = activatePane(visitedA3, "pane-b", "tab-b2");
    const next = closeWorkspaceTab(visitedB2, "pane-a", "tab-a3");

    expect(next.panes.find((pane) => pane.id === "pane-a")?.activeTabId).toBe(
      "tab-a1",
    );
    expect(
      next.panes.find((pane) => pane.id === "pane-b")?.tabVisitHistory,
    ).toEqual(["tab-b2", "tab-b1"]);
  });

  it("returns to the most recently visited tab when the active tab moves into a split", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane(
        "pane-a",
        [
          makeSessionTab("tab-a", "session-a"),
          makeSessionTab("tab-b", "session-b"),
          makeSessionTab("tab-c", "session-c"),
        ],
        {
          activeTabId: "tab-c",
          activeSessionId: "session-c",
          tabVisitHistory: ["tab-c", "tab-b", "tab-a"],
        },
      ),
    );

    const next = splitPane(workspace, "pane-a", "row");
    const sourcePane = next.panes.find((pane) => pane.id === "pane-a");
    const splitPaneState = next.panes.find((pane) => pane.id !== "pane-a");

    expect(sourcePane?.activeTabId).toBe("tab-b");
    expect(sourcePane?.tabVisitHistory).toEqual(["tab-b", "tab-a"]);
    expect(splitPaneState?.activeTabId).toBe("tab-c");
    expect(splitPaneState?.tabVisitHistory).toBeUndefined();
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

  it("openSourceInWorkspaceState creates a viewer pane beside the current session pane", () => {
    const next = openSourceInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
      "/tmp/app.ts",
      "pane-a",
      "session-a",
    );

    const viewerPane = next.panes.find((pane) => pane.id !== "pane-a");

    expect(next.panes).toHaveLength(2);
    expect(next.panes.find((pane) => pane.id === "pane-a")?.tabs).toEqual([
      makeSessionTab("tab-a", "session-a"),
    ]);
    expect(next.activePaneId).toBe(viewerPane?.id);
    expect(viewerPane?.tabs).toEqual([
      {
        id: expect.any(String),
        kind: "source",
        path: "/tmp/app.ts",
        originSessionId: "session-a",
      },
    ]);
    expect(viewerPane).toMatchObject({
      activeSessionId: "session-a",
      viewMode: "source",
      sourcePath: "/tmp/app.ts",
    });
    expect(next.lastContentPaneId).toBe("pane-a");
    expect(next.lastViewerPaneId).toBe(viewerPane?.id);
    expect(next.root).toMatchObject({
      type: "split",
      direction: "row",
      first: { type: "pane", paneId: "pane-a" },
      second: { type: "pane", paneId: viewerPane?.id },
    });
  });

  it("openSourceInWorkspaceState creates a viewer beside the last content pane", () => {
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
    if (!sourcePane) {
      throw new Error("sourcePane not found");
    }
    expect(next.activePaneId).toBe(sourcePane.id);
    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      tabs: [makeControlPanelTab("control-a", null)],
      activeTabId: "control-a",
      viewMode: "controlPanel",
      activeSessionId: null,
    });
    expect(sourcePane.id).not.toBe("pane-b");
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      makeSessionTab("tab-b", "session-b"),
    ]);
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
    expect(next.lastContentPaneId).toBe("pane-b");
    expect(next.lastViewerPaneId).toBe(sourcePane.id);
  });

  it("resolveWorkspaceViewerSplitAnchorPaneId measures the content target rather than a control-surface opener", () => {
    const controlPane = makePane(
      "pane-control",
      [makeFilesystemTab("files-a", "/tmp", "session-a")],
      {
        activeTabId: "files-a",
        activeSessionId: "session-a",
        viewMode: "filesystem",
      },
    );
    const sessionPane = makePane("pane-session", [
      makeSessionTab("tab-a", "session-a"),
    ]);
    const workspace: WorkspaceState = {
      ...makeSplitWorkspace(controlPane, sessionPane, controlPane.id),
      lastContentPaneId: sessionPane.id,
    };

    expect(
      resolveWorkspaceViewerSplitAnchorPaneId(
        workspace,
        controlPane.id,
        "session-a",
      ),
    ).toBe(sessionPane.id);
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

  it("openSourceInWorkspaceState reuses the existing viewer pane", () => {
    const sessionPane = makePane(
      "pane-session",
      [makeSessionTab("tab-session", "session-a")],
      {
        activeTabId: "tab-session",
        activeSessionId: "session-a",
        viewMode: "session",
      },
    );
    const viewerPane = makePane(
      "pane-viewer",
      [makeSourceTab("source-a", "/tmp/app.ts", "session-a")],
      {
        activeTabId: "source-a",
        activeSessionId: "session-a",
        viewMode: "source",
        sourcePath: "/tmp/app.ts",
      },
    );
    const workspace: WorkspaceState = {
      ...makeSplitWorkspace(sessionPane, viewerPane, sessionPane.id),
      lastContentPaneId: sessionPane.id,
      lastViewerPaneId: viewerPane.id,
    };

    const next = openSourceInWorkspaceState(
      workspace,
      "/tmp/next.ts",
      sessionPane.id,
      "session-a",
    );

    expect(next.panes).toHaveLength(2);
    expect(next.activePaneId).toBe(viewerPane.id);
    expect(next.lastContentPaneId).toBe(sessionPane.id);
    expect(next.lastViewerPaneId).toBe(viewerPane.id);
    expect(next.panes.find((pane) => pane.id === sessionPane.id)?.tabs).toEqual(
      [makeSessionTab("tab-session", "session-a")],
    );
    expect(next.panes.find((pane) => pane.id === viewerPane.id)?.tabs).toEqual([
      makeSourceTab("source-a", "/tmp/app.ts", "session-a"),
      {
        id: expect.any(String),
        kind: "source",
        path: "/tmp/next.ts",
        originSessionId: "session-a",
      },
    ]);
  });

  it("openSourceInWorkspaceState creates a viewer beside a legacy mixed primary pane", () => {
    const mixedPane = makePane(
      "pane-primary",
      [
        makeSessionTab("tab-session", "session-a"),
        makeSourceTab("source-old", "/tmp/old.ts", "session-a"),
      ],
      {
        activeTabId: "tab-session",
        activeSessionId: "session-a",
        viewMode: "session",
      },
    );

    const next = openSourceInWorkspaceState(
      makeSinglePaneWorkspace(mixedPane),
      "/tmp/new.ts",
      mixedPane.id,
      "session-a",
    );
    const viewerPane = next.panes.find((pane) => pane.id !== mixedPane.id);

    expect(next.panes).toHaveLength(2);
    expect(next.panes.find((pane) => pane.id === mixedPane.id)?.tabs).toEqual(
      mixedPane.tabs,
    );
    expect(viewerPane?.tabs).toEqual([
      {
        id: expect.any(String),
        kind: "source",
        path: "/tmp/new.ts",
        originSessionId: "session-a",
      },
    ]);
    expect(next.activePaneId).toBe(viewerPane?.id);
    expect(next.lastContentPaneId).toBe(mixedPane.id);
    expect(next.lastViewerPaneId).toBe(viewerPane?.id);
  });

  it("openSourceInWorkspaceState reuses a non-session pane when the opener is too narrow to split", () => {
    const sessionPane = makePane("pane-session", [
      makeSessionTab("tab-session", "session-a"),
    ]);
    const boardPane = makePane(
      "pane-board",
      [makeResponseBoardTab("board-a", "session-a")],
      {
        activeTabId: "board-a",
        activeSessionId: "session-a",
        viewMode: "responseBoard",
      },
    );
    const workspace: WorkspaceState = {
      ...makeSplitWorkspace(sessionPane, boardPane, sessionPane.id),
      lastContentPaneId: sessionPane.id,
    };

    const next = openSourceInWorkspaceState(
      workspace,
      "/tmp/new.ts",
      sessionPane.id,
      "session-a",
      { allowViewerSplit: false },
    );

    expect(next.panes).toHaveLength(2);
    expect(next.root).toEqual(workspace.root);
    expect(next.activePaneId).toBe(boardPane.id);
    expect(next.lastContentPaneId).toBe(sessionPane.id);
    expect(next.lastViewerPaneId).toBe(boardPane.id);
    expect(next.panes.find((pane) => pane.id === sessionPane.id)?.tabs).toEqual(
      [makeSessionTab("tab-session", "session-a")],
    );
    expect(next.panes.find((pane) => pane.id === boardPane.id)?.tabs).toEqual([
      makeResponseBoardTab("board-a", "session-a"),
      {
        id: expect.any(String),
        kind: "source",
        path: "/tmp/new.ts",
        originSessionId: "session-a",
      },
    ]);
  });

  it("openSourceInWorkspaceState uses an existing second session pane instead of creating a third pane", () => {
    const primaryPane = makePane("pane-primary", [
      makeSessionTab("tab-primary", "session-a"),
    ]);
    const secondaryPane = makePane("pane-secondary", [
      makeSessionTab("tab-secondary", "session-b"),
    ]);
    const workspace: WorkspaceState = {
      ...makeSplitWorkspace(primaryPane, secondaryPane, primaryPane.id),
      lastContentPaneId: primaryPane.id,
    };

    const next = openSourceInWorkspaceState(
      workspace,
      "/tmp/new.ts",
      primaryPane.id,
      "session-a",
    );

    expect(next.panes).toHaveLength(2);
    expect(next.root).toEqual(workspace.root);
    expect(next.activePaneId).toBe(secondaryPane.id);
    expect(next.lastContentPaneId).toBe(primaryPane.id);
    expect(next.lastViewerPaneId).toBe(secondaryPane.id);
    expect(next.panes.find((pane) => pane.id === primaryPane.id)?.tabs).toEqual(
      [makeSessionTab("tab-primary", "session-a")],
    );
    expect(
      next.panes.find((pane) => pane.id === secondaryPane.id)?.tabs,
    ).toEqual([
      makeSessionTab("tab-secondary", "session-b"),
      {
        id: expect.any(String),
        kind: "source",
        path: "/tmp/new.ts",
        originSessionId: "session-a",
      },
    ]);
  });

  it("openSourceInWorkspaceState stacks in the opener when a narrow workspace has no reusable pane", () => {
    const next = openSourceInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-session", [makeSessionTab("tab-a", "session-a")]),
      ),
      "/tmp/new.ts",
      "pane-session",
      "session-a",
      { allowViewerSplit: false },
    );

    expect(next.panes).toHaveLength(1);
    expect(next.activePaneId).toBe("pane-session");
    expect(next.lastContentPaneId).toBe("pane-session");
    expect(next.lastViewerPaneId).toBeNull();
    expect(next.panes[0]?.tabs).toEqual([
      makeSessionTab("tab-a", "session-a"),
      {
        id: expect.any(String),
        kind: "source",
        path: "/tmp/new.ts",
        originSessionId: "session-a",
      },
    ]);
  });

  it("openSourceInWorkspaceState preserves a remembered viewer role after a session is dragged into it", () => {
    const primaryPane = makePane("pane-primary", [
      makeSessionTab("tab-primary", "session-a"),
    ]);
    const mixedViewerPane = makePane(
      "pane-viewer",
      [
        makeSourceTab("source-a", "/tmp/app.ts", "session-a"),
        makeSessionTab("tab-review", "session-review"),
      ],
      {
        activeTabId: "tab-review",
        activeSessionId: "session-review",
        viewMode: "session",
      },
    );
    const workspace: WorkspaceState = {
      ...makeSplitWorkspace(primaryPane, mixedViewerPane, primaryPane.id),
      lastContentPaneId: primaryPane.id,
      lastViewerPaneId: mixedViewerPane.id,
    };

    const next = openSourceInWorkspaceState(
      workspace,
      "/tmp/next.ts",
      primaryPane.id,
      "session-a",
    );

    expect(next.panes).toHaveLength(2);
    expect(next.activePaneId).toBe(mixedViewerPane.id);
    expect(
      next.panes.find((pane) => pane.id === mixedViewerPane.id)?.tabs,
    ).toEqual([
      makeSourceTab("source-a", "/tmp/app.ts", "session-a"),
      makeSessionTab("tab-review", "session-review"),
      {
        id: expect.any(String),
        kind: "source",
        path: "/tmp/next.ts",
        originSessionId: "session-a",
      },
    ]);
    expect(next.lastContentPaneId).toBe(primaryPane.id);
    expect(next.lastViewerPaneId).toBe(mixedViewerPane.id);
  });

  it("openSourceInWorkspaceState recreates a viewer after its last tab closes", () => {
    const opened = openSourceInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-session", [makeSessionTab("tab-a", "session-a")]),
      ),
      "/tmp/app.ts",
      "pane-session",
      "session-a",
    );
    const firstViewer = opened.panes.find((pane) => pane.id !== "pane-session");
    const firstViewerTab = firstViewer?.tabs[0];
    if (!firstViewer || !firstViewerTab) {
      throw new Error("Expected the first viewer pane");
    }

    const withoutViewer = closeWorkspaceTab(
      opened,
      firstViewer.id,
      firstViewerTab.id,
    );
    const next = openSourceInWorkspaceState(
      withoutViewer,
      "/tmp/next.ts",
      "pane-session",
      "session-a",
    );
    const recreatedViewer = next.panes.find(
      (pane) => pane.id !== "pane-session",
    );

    expect(next.panes).toHaveLength(2);
    expect(recreatedViewer?.id).not.toBe(firstViewer.id);
    expect(next.activePaneId).toBe(recreatedViewer?.id);
    expect(next.lastContentPaneId).toBe("pane-session");
    expect(next.lastViewerPaneId).toBe(recreatedViewer?.id);
    expect(recreatedViewer?.tabs).toEqual([
      {
        id: expect.any(String),
        kind: "source",
        path: "/tmp/next.ts",
        originSessionId: "session-a",
      },
    ]);
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

  it("openSourceInWorkspaceState activates an existing source tab without moving it", () => {
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

    expect(next.panes).toHaveLength(2);
    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      activeSessionId: "session-a",
      activeTabId: "source-a",
      viewMode: "source",
      sourcePath: "/tmp/app.ts",
    });
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      makeSessionTab("tab-b", "session-a"),
    ]);
  });

  it("openSourceInWorkspaceState reuses a restored source tab with a legacy Windows UNC verbatim path", () => {
    const legacyPath = String.raw`\\?\UNC\server\share\src\app.ts`;
    const normalizedPath = String.raw`\\server\share\src\app.ts`;
    const paneA = makePane(
      "pane-a",
      [makeSourceTab("source-a", legacyPath, "session-a")],
      {
        activeTabId: "source-a",
        activeSessionId: "session-a",
        viewMode: "source",
        sourcePath: legacyPath,
      },
    );
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openSourceInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      normalizedPath,
      paneA.id,
      "session-a",
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")?.activeTabId).toBe(
      "source-a",
    );
    expect(next.panes.find((pane) => pane.id === "pane-a")?.tabs).toEqual([
      makeSourceTab("source-a", legacyPath, "session-a"),
    ]);
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      makeSessionTab("tab-b", "session-b"),
    ]);
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

  it("openTerminalInWorkspaceState creates a terminal tab and switches the pane mode", () => {
    const next = openTerminalInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
      "/tmp/project",
      "pane-a",
      "session-a",
      "project-a",
    );

    expect(next.panes[0].tabs).toHaveLength(2);
    expect(next.panes[0].tabs[1]).toEqual({
      id: expect.any(String),
      kind: "terminal",
      workdir: "/tmp/project",
      originSessionId: "session-a",
      originProjectId: "project-a",
    });
    expect(next.panes[0].viewMode).toBe("terminal");
    expect(next.panes[0].activeSessionId).toBe("session-a");
  });

  it("openTerminalInWorkspaceState focuses an existing terminal tab for the same workdir and scope", () => {
    const paneA = makePane(
      "pane-a",
      [makeTerminalTab("terminal-a", "/tmp/project", "session-a", "project-a")],
      {
        activeTabId: "terminal-a",
        activeSessionId: "session-a",
        viewMode: "terminal",
      },
    );
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openTerminalInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "/tmp/project",
      paneB.id,
      "session-a",
      "project-a",
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes.find((pane) => pane.id === "pane-a")?.activeTabId).toBe(
      "terminal-a",
    );
  });

  it("openTerminalInWorkspaceState keeps terminal tabs distinct by session and project scope", () => {
    const paneA = makePane(
      "pane-a",
      [makeTerminalTab("terminal-a", "/tmp/project", "session-a", "project-a")],
      {
        activeTabId: "terminal-a",
        activeSessionId: "session-a",
        viewMode: "terminal",
      },
    );
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openTerminalInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      "/tmp/project",
      paneB.id,
      "session-b",
      "project-b",
    );

    const targetPane = next.panes.find((pane) => pane.id === "pane-b");
    expect(next.activePaneId).toBe("pane-b");
    expect(targetPane?.tabs).toHaveLength(2);
    expect(targetPane?.tabs[1]).toEqual({
      id: expect.any(String),
      kind: "terminal",
      workdir: "/tmp/project",
      originSessionId: "session-b",
      originProjectId: "project-b",
    });
  });

  it("openFilesystemInWorkspaceState reuses a restored filesystem tab with a legacy Windows verbatim root", () => {
    const legacyRoot = String.raw`\\?\C:\repo`;
    const normalizedRoot = String.raw`C:\repo`;
    const paneA = makePane(
      "pane-a",
      [makeFilesystemTab("fs-a", legacyRoot, "session-a")],
      {
        activeTabId: "fs-a",
        activeSessionId: "session-a",
        viewMode: "filesystem",
      },
    );
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openFilesystemInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      normalizedRoot,
      paneB.id,
      "session-b",
    );

    const filesystemPane = next.panes.find((pane) => pane.id === "pane-a");
    expect(next.activePaneId).toBe("pane-a");
    expect(filesystemPane?.activeTabId).toBe("fs-a");
    expect(filesystemPane?.tabs).toEqual([
      makeFilesystemTab("fs-a", legacyRoot, "session-a"),
    ]);
  });

  it("openGitStatusInWorkspaceState reuses a restored git status tab with a legacy Windows verbatim workdir", () => {
    const legacyWorkdir = String.raw`\\?\C:\repo`;
    const normalizedWorkdir = String.raw`C:\repo`;
    const paneA = makePane(
      "pane-a",
      [makeGitStatusTab("git-a", legacyWorkdir, "session-a")],
      {
        activeTabId: "git-a",
        activeSessionId: "session-a",
        viewMode: "gitStatus",
      },
    );
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openGitStatusInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      normalizedWorkdir,
      paneB.id,
      "session-b",
    );

    const gitPane = next.panes.find((pane) => pane.id === "pane-a");
    expect(next.activePaneId).toBe("pane-a");
    expect(gitPane?.activeTabId).toBe("git-a");
    expect(gitPane?.tabs).toEqual([
      makeGitStatusTab("git-a", legacyWorkdir, "session-a"),
    ]);
  });

  it("openTerminalInWorkspaceState reuses a restored terminal tab with a legacy Windows verbatim workdir", () => {
    const legacyWorkdir = String.raw`\\?\C:\repo`;
    const normalizedWorkdir = String.raw`C:\repo`;
    const paneA = makePane(
      "pane-a",
      [makeTerminalTab("terminal-a", legacyWorkdir, "session-a", "project-a")],
      {
        activeTabId: "terminal-a",
        activeSessionId: "session-a",
        viewMode: "terminal",
      },
    );
    const paneB = makePane("pane-b", [makeSessionTab("tab-b", "session-b")]);

    const next = openTerminalInWorkspaceState(
      makeSplitWorkspace(paneA, paneB, paneB.id),
      normalizedWorkdir,
      paneB.id,
      "session-a",
      "project-a",
    );

    const terminalPane = next.panes.find((pane) => pane.id === "pane-a");
    expect(next.activePaneId).toBe("pane-a");
    expect(terminalPane?.activeTabId).toBe("terminal-a");
    expect(terminalPane?.tabs).toEqual([
      makeTerminalTab("terminal-a", legacyWorkdir, "session-a", "project-a"),
    ]);
  });

  it("openTerminalInWorkspaceState creates separate null-workdir terminal tabs", () => {
    const pane = makePane("pane-a", [
      makeSessionTab("tab-a", "session-a"),
      makeTerminalTab("terminal-a", null, "session-a", "project-a"),
    ]);

    const next = openTerminalInWorkspaceState(
      makeSinglePaneWorkspace(pane),
      null,
      "pane-a",
      "session-a",
      "project-a",
    );

    expect(next.panes[0].tabs).toHaveLength(3);
    expect(next.panes[0].tabs[2]).toEqual({
      id: expect.any(String),
      kind: "terminal",
      workdir: null,
      originSessionId: "session-a",
      originProjectId: "project-a",
    });
    expect(next.panes[0].activeTabId).toBe(next.panes[0].tabs[2]?.id ?? null);
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

  it("openSessionListInWorkspaceState refreshes origin metadata when reusing an existing sessions tab", () => {
    const next = openSessionListInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane(
          "pane-a",
          [
            makeSessionTab("tab-a", "session-a"),
            makeSessionListTab("sessions-a", null),
          ],
          {
            activeTabId: "tab-a",
            activeSessionId: "session-a",
            viewMode: "session",
          },
        ),
      ),
      "pane-a",
      "session-a",
      "project-a",
    );

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes[0]).toMatchObject({
      activeTabId: "sessions-a",
      activeSessionId: "session-a",
      viewMode: "sessionList",
    });
    expect(next.panes[0].tabs[1]).toEqual({
      id: "sessions-a",
      kind: "sessionList",
      originSessionId: "session-a",
      originProjectId: "project-a",
    });
  });

  it("openSessionListInWorkspaceState updates and activates the existing tab in place", () => {
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

    expect(next.activePaneId).toBe("pane-a");
    expect(next.panes).toHaveLength(2);
    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      activeTabId: "sessions-a",
      activeSessionId: "session-b",
      viewMode: "sessionList",
      tabs: [makeSessionListTab("sessions-a", "session-b", "project-b")],
    });
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      makeSessionTab("tab-b", "session-b"),
    ]);
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

  it("openDiffPreviewInWorkspaceState creates a viewer pane beside the current pane", () => {
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

    const diffPane = next.panes.find((pane) => pane.id !== "pane-a");

    expect(next.panes).toHaveLength(2);
    expect(next.panes.find((pane) => pane.id === "pane-a")?.tabs).toEqual([
      makeSessionTab("tab-a", "session-a"),
    ]);
    expect(diffPane).toMatchObject({
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
    expect(next.activePaneId).toBe(diffPane?.id);
    expect(next.lastContentPaneId).toBe("pane-a");
    expect(next.lastViewerPaneId).toBe(diffPane?.id);
  });

  it("openDiffPreviewInWorkspaceState preserves a reusable pane's existing tab when there is no room to split", () => {
    const sessionPane = makePane("pane-session", [
      makeSessionTab("tab-session", "session-a"),
    ]);
    const boardPane = makePane(
      "pane-board",
      [makeResponseBoardTab("board-a", "session-a")],
      {
        activeTabId: "board-a",
        activeSessionId: "session-a",
        viewMode: "responseBoard",
      },
    );
    const workspace: WorkspaceState = {
      ...makeSplitWorkspace(sessionPane, boardPane, sessionPane.id),
      lastContentPaneId: sessionPane.id,
    };

    const next = openDiffPreviewInWorkspaceState(
      workspace,
      {
        changeType: "edit",
        diff: "-before\n+after",
        diffMessageId: "diff-a",
        filePath: "/tmp/app.ts",
        language: "typescript",
        originSessionId: "session-a",
        summary: "Updated app.ts",
      },
      sessionPane.id,
      {
        reuseActiveViewerTab: true,
        allowViewerSplit: false,
      },
    );

    expect(next.panes).toHaveLength(2);
    expect(next.root).toEqual(workspace.root);
    expect(next.activePaneId).toBe(boardPane.id);
    expect(next.lastContentPaneId).toBe(sessionPane.id);
    expect(next.lastViewerPaneId).toBe(boardPane.id);
    expect(next.panes.find((pane) => pane.id === boardPane.id)?.tabs).toEqual([
      makeResponseBoardTab("board-a", "session-a"),
      {
        id: expect.any(String),
        kind: "diffPreview",
        changeType: "edit",
        diff: "-before\n+after",
        diffMessageId: "diff-a",
        filePath: "/tmp/app.ts",
        language: "typescript",
        originSessionId: "session-a",
        summary: "Updated app.ts",
      },
    ]);
  });

  it("openDiffPreviewInWorkspaceState preserves Markdown document content metadata", () => {
    const documentContent = {
      before: {
        content: "# Before\n",
        source: "index" as const,
      },
      after: {
        content: "# After\n",
        source: "worktree" as const,
      },
      canEdit: true,
      isCompleteDocument: true,
    };
    const next = openDiffPreviewInWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-a", "session-a")]),
      ),
      {
        changeType: "edit",
        diff: "-# Before\n+# After",
        documentEnrichmentNote: "  Rendered from full document sides.  ",
        documentContent,
        diffMessageId: "git-preview:pane-a:/repo:unstaged::README.md",
        filePath: "/repo/README.md",
        gitDiffRequest: {
          path: "README.md",
          sectionId: "unstaged",
          workdir: "/repo",
        },
        gitDiffRequestKey: "git-preview:pane-a:/repo:unstaged::README.md",
        gitSectionId: "unstaged",
        language: "markdown",
        originSessionId: "session-a",
        summary: "Updated README",
      },
      "pane-a",
    );

    const diffTab = next.panes
      .flatMap((pane) => pane.tabs)
      .find((tab) => tab.kind === "diffPreview");

    expect(diffTab).toMatchObject({
      kind: "diffPreview",
      documentEnrichmentNote: "  Rendered from full document sides.  ",
      documentContent,
      gitDiffRequest: {
        path: "README.md",
        sectionId: "unstaged",
        workdir: "/repo",
      },
      gitDiffRequestKey: "git-preview:pane-a:/repo:unstaged::README.md",
      language: "markdown",
    });
  });

  it("stripLoadingGitDiffPreviewTabsFromWorkspaceState removes empty transient Git diff previews", () => {
    const sessionTab = makeSessionTab("tab-a", "session-a");
    const loadingTab: WorkspaceTab = {
      id: "diff-tab-a",
      kind: "diffPreview",
      changeType: "edit",
      diff: "",
      diffMessageId: "git-preview:pane-a:/repo:unstaged::README.md",
      filePath: "/repo/README.md",
      gitDiffRequest: {
        path: "README.md",
        sectionId: "unstaged",
        workdir: "/repo",
      },
      gitDiffRequestKey: "git-preview:pane-a:/repo:unstaged::README.md",
      gitSectionId: "unstaged",
      isLoading: true,
      language: "markdown",
      originSessionId: "session-a",
      summary: "Updated file",
    };
    const next = stripLoadingGitDiffPreviewTabsFromWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [loadingTab, sessionTab], {
          activeTabId: loadingTab.id,
          activeSessionId: "session-a",
          viewMode: "diffPreview",
        }),
      ),
    );

    expect(next.panes[0]?.tabs).toEqual([sessionTab]);
    expect(next.panes[0]).toMatchObject({
      activeTabId: sessionTab.id,
      viewMode: "session",
    });
  });

  it("stripLoadingGitDiffPreviewTabsFromWorkspaceState keeps restored Git diff previews with durable diff text", () => {
    const loadingRestoredTab: WorkspaceTab = {
      id: "diff-tab-a",
      kind: "diffPreview",
      changeType: "edit",
      diff: "-before\n+after",
      diffMessageId: "git-preview:pane-a:/repo:unstaged::README.md",
      filePath: "/repo/README.md",
      gitDiffRequest: {
        path: "README.md",
        sectionId: "unstaged",
        workdir: "/repo",
      },
      gitDiffRequestKey: "git-preview:pane-a:/repo:unstaged::README.md",
      gitSectionId: "unstaged",
      isLoading: true,
      language: "markdown",
      originSessionId: "session-a",
      summary: "Updated file",
    };
    const workspace = makeSinglePaneWorkspace(
      makePane("pane-a", [loadingRestoredTab], {
        activeTabId: loadingRestoredTab.id,
        activeSessionId: "session-a",
        viewMode: "diffPreview",
      }),
    );

    expect(stripLoadingGitDiffPreviewTabsFromWorkspaceState(workspace)).toBe(
      workspace,
    );
  });

  it("openDiffPreviewInWorkspaceState creates a viewer beside the last content pane", () => {
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
    if (!diffPane) {
      throw new Error("diffPane not found");
    }
    expect(next.activePaneId).toBe(diffPane.id);
    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      tabs: [makeControlPanelTab("control-a", null)],
      activeTabId: "control-a",
      viewMode: "controlPanel",
      activeSessionId: null,
    });
    expect(diffPane.id).not.toBe("pane-b");
    expect(next.panes.find((pane) => pane.id === "pane-b")?.tabs).toEqual([
      makeSessionTab("tab-b", "session-b"),
    ]);
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
    expect(next.lastContentPaneId).toBe("pane-b");
    expect(next.lastViewerPaneId).toBe(diffPane.id);
  });

  it("openDiffPreviewInWorkspaceState creates a viewer beside the related content pane", () => {
    const next = openDiffPreviewInWorkspaceState(
      makeSplitWorkspace(
        makePane(
          "pane-git",
          [makeGitStatusTab("git-a", "/repo", "session-a")],
          {
            activeTabId: "git-a",
            activeSessionId: "session-a",
            viewMode: "gitStatus",
          },
        ),
        makePane("pane-session", [makeSessionTab("tab-a", "session-a")], {
          activeTabId: "tab-a",
          activeSessionId: "session-a",
          viewMode: "session",
        }),
        "pane-git",
      ),
      {
        changeType: "edit",
        diff: "-before\n+after",
        diffMessageId: "diff-a",
        filePath: "/repo/src/app.ts",
        language: "typescript",
        originSessionId: "session-a",
        originProjectId: "project-a",
        summary: "Updated app.ts",
      },
      "pane-git",
      {
        reuseActiveViewerTab: true,
      },
    );
    const diffPane = next.panes.find((pane) =>
      pane.tabs.some((tab) => tab.kind === "diffPreview"),
    );

    if (!diffPane) {
      throw new Error("Expected diff preview in a content pane");
    }

    expect(next.panes).toHaveLength(3);
    expect(next.activePaneId).toBe(diffPane.id);
    expect(diffPane.id).not.toBe("pane-session");
    expect(next.panes.find((pane) => pane.id === "pane-git")).toMatchObject({
      tabs: [makeGitStatusTab("git-a", "/repo", "session-a")],
      activeTabId: "git-a",
      viewMode: "gitStatus",
      activeSessionId: "session-a",
    });
    expect(next.panes.find((pane) => pane.id === "pane-session")?.tabs).toEqual(
      [makeSessionTab("tab-a", "session-a")],
    );
    expect(diffPane).toMatchObject({
      tabs: [
        {
          id: expect.any(String),
          kind: "diffPreview",
          diffMessageId: "diff-a",
          filePath: "/repo/src/app.ts",
          originSessionId: "session-a",
          originProjectId: "project-a",
          summary: "Updated app.ts",
        },
      ],
      viewMode: "diffPreview",
    });
    expect(next.lastContentPaneId).toBe("pane-session");
    expect(next.lastViewerPaneId).toBe(diffPane.id);
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

    const diffPane = next.panes.find((pane) => pane.id === "pane-b");
    if (!diffPane) {
      throw new Error("Diff preview pane not found");
    }

    expect(next.activePaneId).toBe(diffPane.id);
    expect(diffPane.tabs).toEqual([
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

  it("openDiffPreviewInWorkspaceState sends a new preview to the existing viewer pane", () => {
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
    expect(next.panes.find((pane) => pane.id === "pane-a")?.tabs).toEqual([
      makeSessionTab("tab-a", "session-a"),
    ]);
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
            tabVisitHistory: ["diff-tab-a"],
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
    const replacementTabId = next.panes.find((pane) => pane.id === "pane-b")
      ?.tabs[0]?.id;
    expect(replacementTabId).not.toBe("diff-tab-a");
    expect(
      next.panes.find((pane) => pane.id === "pane-b")?.tabVisitHistory,
    ).toEqual([replacementTabId]);
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

  it("openDiffPreviewInWorkspaceState prefers the remembered viewer pane over tree order", () => {
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
      lastContentPaneId: rightViewerPane.id,
      lastViewerPaneId: rightViewerPane.id,
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

  it("openDiffPreviewInWorkspaceState uses the existing viewer pane from the control panel", () => {
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
      activePaneId: controlPane.id,
      lastContentPaneId: localSessionPane.id,
      lastViewerPaneId: farViewerPane.id,
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

    expect(next.activePaneId).toBe(farViewerPane.id);
    expect(
      next.panes.find((pane) => pane.id === localSessionPane.id)?.tabs,
    ).toEqual([makeSessionTab("session-b", "session-b")]);
    expect(
      next.panes.find((pane) => pane.id === farViewerPane.id)?.tabs,
    ).toEqual([
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

  it("placeDraggedTab preserves source and target pane visit histories", () => {
    const sourcePane = makePane(
      "pane-a",
      [
        makeSessionTab("tab-a1", "session-a1"),
        makeSessionTab("tab-a2", "session-a2"),
        makeSessionTab("tab-a3", "session-a3"),
      ],
      {
        activeTabId: "tab-a3",
        activeSessionId: "session-a3",
        tabVisitHistory: ["tab-a3", "tab-a2", "tab-a1"],
      },
    );
    const targetPane = makePane(
      "pane-b",
      [
        makeSessionTab("tab-b1", "session-b1"),
        makeSessionTab("tab-b2", "session-b2"),
      ],
      {
        activeTabId: "tab-b1",
        activeSessionId: "session-b1",
        tabVisitHistory: ["tab-b1", "tab-b2"],
      },
    );

    const next = placeDraggedTab(
      makeSplitWorkspace(sourcePane, targetPane),
      "pane-a",
      "tab-a3",
      "pane-b",
      "tabs",
      1,
    );

    expect(next.panes.find((pane) => pane.id === "pane-a")).toMatchObject({
      activeTabId: "tab-a2",
      activeSessionId: "session-a2",
      tabVisitHistory: ["tab-a2", "tab-a1"],
    });
    expect(next.panes.find((pane) => pane.id === "pane-b")).toMatchObject({
      activeTabId: "tab-a3",
      activeSessionId: "session-a3",
      tabVisitHistory: ["tab-a3", "tab-b1", "tab-b2"],
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

  it("reconcileWorkspaceState returns to the most recently visited surviving tab", () => {
    const next = reconcileWorkspaceState(
      makeSinglePaneWorkspace(
        makePane(
          "pane-a",
          [
            makeSessionTab("tab-a", "session-a"),
            makeSessionTab("tab-b", "session-b"),
            makeSessionTab("tab-c", "session-c"),
          ],
          {
            activeTabId: "tab-c",
            activeSessionId: "session-c",
            tabVisitHistory: ["tab-c", "tab-b", "tab-a"],
          },
        ),
      ),
      [makeSession("session-a"), makeSession("session-b")],
    );

    expect(next.panes[0].activeTabId).toBe("tab-b");
    expect(next.panes[0].activeSessionId).toBe("session-b");
    expect(next.panes[0].tabVisitHistory).toEqual(["tab-b", "tab-a"]);
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

  it("reconcileWorkspaceState prunes delegated child workspace entries when restart recovery requests it", () => {
    const childSession = {
      ...makeSession("session-child"),
      parentDelegationId: "delegation-1",
    };
    const next = reconcileWorkspaceState(
      makeSinglePaneWorkspace(
        makePane(
          "pane-a",
          [
            makeSessionTab("tab-child", "session-child"),
            makeSessionTab("tab-parent", "session-parent"),
            makeSourceTab("source-child", "/tmp/review.md", "session-child"),
            makeCanvasTab(
              "canvas-a",
              [
                { sessionId: "session-child", x: 10, y: 20 },
                { sessionId: "session-parent", x: 30, y: 40 },
              ],
              "session-child",
            ),
          ],
          {
            activeTabId: "tab-child",
            activeSessionId: "session-child",
            viewMode: "session",
          },
        ),
      ),
      [makeSession("session-parent"), childSession],
      { pruneDelegatedChildSessionTabs: true },
    );

    expect(next.panes[0].tabs).toEqual([
      makeSessionTab("tab-parent", "session-parent"),
      makeSourceTab("source-child", "/tmp/review.md", null),
      makeCanvasTab(
        "canvas-a",
        [{ sessionId: "session-parent", x: 30, y: 40 }],
        null,
      ),
    ]);
    expect(next.panes[0].activeSessionId).toBe("session-parent");
  });

  it("detects delegated child references from canvas origin sessions without cards", () => {
    const childSession = {
      ...makeSession("session-child"),
      parentDelegationId: "delegation-1",
    };
    const workspace = makeSinglePaneWorkspace(
      makePane("pane-a", [makeCanvasTab("canvas-a", [], "session-child")], {
        activeTabId: "canvas-a",
        activeSessionId: null,
        viewMode: "canvas",
      }),
    );

    expect(
      workspaceHasDelegatedChildSessionReferences(workspace, [
        makeSession("session-parent"),
        childSession,
      ]),
    ).toBe(true);
  });

  it("collects workspace session references from tabs, origins, active panes, and canvas cards", () => {
    const workspace = makeSinglePaneWorkspace(
      makePane(
        "pane-a",
        [
          makeSessionTab("tab-session", "session-tab"),
          makeSourceTab("tab-source", "/tmp/review.md", "session-origin"),
          makeCanvasTab(
            "tab-canvas",
            [{ sessionId: "session-card", x: 10, y: 20 }],
            "session-canvas-origin",
          ),
        ],
        {
          activeTabId: "tab-session",
          activeSessionId: "session-active",
          viewMode: "session",
        },
      ),
    );

    expect([...collectWorkspaceSessionReferences(workspace)].sort()).toEqual([
      "session-active",
      "session-canvas-origin",
      "session-card",
      "session-origin",
      "session-tab",
    ]);
  });

  it("reconcileWorkspaceState keeps delegated child workspace entries during ordinary updates", () => {
    const childSession = {
      ...makeSession("session-child"),
      parentDelegationId: "delegation-1",
    };
    const next = reconcileWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-child", "session-child")], {
          activeTabId: "tab-child",
          activeSessionId: "session-child",
          viewMode: "session",
        }),
      ),
      [childSession],
    );

    expect(next.panes[0].tabs).toEqual([
      makeSessionTab("tab-child", "session-child"),
    ]);
    expect(next.panes[0].activeSessionId).toBe("session-child");
  });

  it("reconcileWorkspaceState preserves selected delegated children during restart pruning", () => {
    const restoredChildSession = {
      ...makeSession("session-restored-child"),
      parentDelegationId: "delegation-1",
    };
    const currentChildSession = {
      ...makeSession("session-current-child"),
      parentDelegationId: "delegation-2",
    };
    const next = reconcileWorkspaceState(
      makeSinglePaneWorkspace(
        makePane(
          "pane-a",
          [
            makeSessionTab("tab-restored", restoredChildSession.id),
            makeSessionTab("tab-current", currentChildSession.id),
            makeSessionTab("tab-parent", "session-parent"),
          ],
          {
            activeTabId: "tab-restored",
            activeSessionId: restoredChildSession.id,
            viewMode: "session",
          },
        ),
      ),
      [
        restoredChildSession,
        currentChildSession,
        makeSession("session-parent"),
      ],
      {
        pruneDelegatedChildSessionTabs: true,
        preserveSessionIds: [currentChildSession.id],
      },
    );

    const sessionIds = next.panes[0].tabs.flatMap((tab) =>
      tab.kind === "session" ? [tab.sessionId] : [],
    );
    expect(sessionIds).toEqual([currentChildSession.id, "session-parent"]);
    expect(next.panes[0].activeSessionId).toBe(currentChildSession.id);
  });

  it("reconcileWorkspaceState rebuilds fallback tabs when pruning empties the restored pane", () => {
    const childSession = {
      ...makeSession("session-child"),
      parentDelegationId: "delegation-1",
    };
    const next = reconcileWorkspaceState(
      makeSinglePaneWorkspace(
        makePane("pane-a", [makeSessionTab("tab-child", "session-child")], {
          activeTabId: "tab-child",
          activeSessionId: "session-child",
          viewMode: "session",
        }),
      ),
      [childSession, makeSession("session-parent")],
      { pruneDelegatedChildSessionTabs: true },
    );

    expect(next.panes).toHaveLength(1);
    expect(next.panes[0].tabs).toEqual([
      expect.objectContaining({
        kind: "session",
        sessionId: "session-parent",
      }),
    ]);
    expect(next.panes[0].activeSessionId).toBe("session-parent");
    expect(next.root).toEqual({
      type: "pane",
      paneId: next.panes[0].id,
    });
  });

  it("reconcileWorkspaceState preserves pre-existing empty panes during restart pruning", () => {
    const childSession = {
      ...makeSession("session-child"),
      parentDelegationId: "delegation-1",
    };
    const next = reconcileWorkspaceState(
      makeSplitWorkspace(
        makePane("pane-child", [makeSessionTab("tab-child", "session-child")], {
          activeTabId: "tab-child",
          activeSessionId: "session-child",
          viewMode: "session",
        }),
        makePane("pane-empty", [], {
          activeTabId: null,
          activeSessionId: null,
          viewMode: "session",
        }),
        "pane-empty",
      ),
      [childSession, makeSession("session-parent")],
      { pruneDelegatedChildSessionTabs: true },
    );

    expect(next.panes).toEqual([
      makePane("pane-empty", [], {
        activeTabId: null,
        activeSessionId: null,
        viewMode: "session",
      }),
    ]);
    expect(next.root).toEqual({
      type: "pane",
      paneId: "pane-empty",
    });
    expect(next.activePaneId).toBe("pane-empty");
  });

  it("reconcileWorkspaceState creates fallback tabs from nondelegated sessions during restart pruning", () => {
    const childSession = {
      ...makeSession("session-child"),
      parentDelegationId: "delegation-1",
    };
    const next = reconcileWorkspaceState(
      {
        root: null,
        panes: [],
        activePaneId: null,
      },
      [childSession, makeSession("session-parent")],
      { pruneDelegatedChildSessionTabs: true },
    );

    expect(next.panes).toHaveLength(1);
    expect(next.panes[0].tabs).toEqual([
      expect.objectContaining({
        kind: "session",
        sessionId: "session-parent",
      }),
    ]);
    expect(next.panes[0].activeSessionId).toBe("session-parent");
  });

  it("reconcileWorkspaceState keeps an empty workspace when every session is pruned", () => {
    const next = reconcileWorkspaceState(
      {
        root: null,
        panes: [],
        activePaneId: null,
      },
      [
        {
          ...makeSession("session-child"),
          parentDelegationId: "delegation-1",
        },
      ],
      { pruneDelegatedChildSessionTabs: true },
    );

    expect(next).toEqual({
      root: null,
      panes: [],
      activePaneId: null,
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
