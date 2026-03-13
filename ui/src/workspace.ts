import type { DiffMessage, Session } from "./types";

export type SessionPaneViewMode = "session" | "prompt" | "commands" | "diffs";
export type PaneViewMode =
  | SessionPaneViewMode
  | "controlPanel"
  | "source"
  | "filesystem"
  | "gitStatus"
  | "diffPreview";

export type WorkspaceSessionTab = {
  id: string;
  kind: "session";
  sessionId: string;
};

export type WorkspaceSourceTab = {
  id: string;
  kind: "source";
  path: string | null;
  originSessionId: string | null;
};

export type WorkspaceFilesystemTab = {
  id: string;
  kind: "filesystem";
  rootPath: string | null;
  originSessionId: string | null;
};

export type WorkspaceGitStatusTab = {
  id: string;
  kind: "gitStatus";
  workdir: string | null;
  originSessionId: string | null;
};

export type WorkspaceControlPanelTab = {
  id: string;
  kind: "controlPanel";
  originSessionId: string | null;
};

export type WorkspaceDiffPreviewTab = {
  id: string;
  kind: "diffPreview";
  changeType: DiffMessage["changeType"];
  diff: string;
  diffMessageId: string;
  filePath: string | null;
  language?: string | null;
  originSessionId: string | null;
  summary: string;
};

export type WorkspaceTab =
  | WorkspaceSessionTab
  | WorkspaceSourceTab
  | WorkspaceFilesystemTab
  | WorkspaceGitStatusTab
  | WorkspaceControlPanelTab
  | WorkspaceDiffPreviewTab;

export type WorkspacePane = {
  id: string;
  tabs: WorkspaceTab[];
  activeTabId: string | null;
  activeSessionId: string | null;
  viewMode: PaneViewMode;
  lastSessionViewMode: SessionPaneViewMode;
  sourcePath: string | null;
};

export type WorkspaceNode =
  | {
      type: "pane";
      paneId: string;
    }
  | {
      id: string;
      type: "split";
      direction: "row" | "column";
      ratio: number;
      first: WorkspaceNode;
      second: WorkspaceNode;
    };

export type WorkspaceState = {
  root: WorkspaceNode | null;
  panes: WorkspacePane[];
  activePaneId: string | null;
};

export type TabDropPlacement = "left" | "right" | "top" | "bottom" | "tabs";

export function createSessionTab(sessionId: string): WorkspaceSessionTab {
  return {
    id: crypto.randomUUID(),
    kind: "session",
    sessionId,
  };
}

export function createSourceTab(
  path: string | null = null,
  originSessionId: string | null = null,
): WorkspaceSourceTab {
  return {
    id: crypto.randomUUID(),
    kind: "source",
    path: normalizeWorkspacePath(path),
    originSessionId,
  };
}

export function createFilesystemTab(
  rootPath: string | null = null,
  originSessionId: string | null = null,
): WorkspaceFilesystemTab {
  return {
    id: crypto.randomUUID(),
    kind: "filesystem",
    rootPath: normalizeWorkspacePath(rootPath),
    originSessionId,
  };
}

export function createGitStatusTab(
  workdir: string | null = null,
  originSessionId: string | null = null,
): WorkspaceGitStatusTab {
  return {
    id: crypto.randomUUID(),
    kind: "gitStatus",
    workdir: normalizeWorkspacePath(workdir),
    originSessionId,
  };
}

export function createControlPanelTab(
  originSessionId: string | null = null,
): WorkspaceControlPanelTab {
  return {
    id: crypto.randomUUID(),
    kind: "controlPanel",
    originSessionId,
  };
}

export function createDiffPreviewTab({
  changeType,
  diff,
  diffMessageId,
  filePath = null,
  language = null,
  originSessionId = null,
  summary,
}: {
  changeType: DiffMessage["changeType"];
  diff: string;
  diffMessageId: string;
  filePath?: string | null;
  language?: string | null;
  originSessionId?: string | null;
  summary: string;
}): WorkspaceDiffPreviewTab {
  return {
    id: crypto.randomUUID(),
    kind: "diffPreview",
    changeType,
    diff,
    diffMessageId,
    filePath: normalizeWorkspacePath(filePath),
    language,
    originSessionId,
    summary,
  };
}

export function reconcileWorkspaceState(current: WorkspaceState, sessions: Session[]): WorkspaceState {
  const availableSessionIds = new Set(sessions.map((session) => session.id));
  let panes = current.panes.map((pane) => {
    const tabs = pane.tabs
      .flatMap((tab): WorkspaceTab[] => {
        if (tab.kind === "session") {
          return availableSessionIds.has(tab.sessionId) ? [tab] : [];
        }

        const originSessionId =
          tab.originSessionId && availableSessionIds.has(tab.originSessionId)
            ? tab.originSessionId
            : null;

        if (tab.kind === "source") {
          return [
            {
              ...tab,
              originSessionId,
              path: normalizeWorkspacePath(tab.path),
            },
          ];
        }

        if (tab.kind === "filesystem") {
          return [
            {
              ...tab,
              originSessionId,
              rootPath: normalizeWorkspacePath(tab.rootPath),
            },
          ];
        }

        if (tab.kind === "gitStatus") {
          return [
            {
              ...tab,
              originSessionId,
              workdir: normalizeWorkspacePath(tab.workdir),
            },
          ];
        }

        if (tab.kind === "controlPanel") {
          return [
            {
              ...tab,
              originSessionId,
            },
          ];
        }

        return [
          {
            ...tab,
            originSessionId,
            filePath: normalizeWorkspacePath(tab.filePath),
          },
        ];
      });
    const activeTabId = tabs.some((tab) => tab.id === pane.activeTabId)
      ? pane.activeTabId
      : (tabs[0]?.id ?? null);
    const activeSessionId =
      pane.activeSessionId && availableSessionIds.has(pane.activeSessionId)
        ? pane.activeSessionId
        : null;

    return syncPaneState({
      ...pane,
      tabs,
      activeTabId,
      activeSessionId,
    });
  });

  let root = pruneWorkspaceNode(current.root, new Set(panes.map((pane) => pane.id)));

  if (!root && panes.length > 0) {
    root = {
      type: "pane",
      paneId: panes[0].id,
    };
  }

  if (!root && sessions.length > 0) {
    const initialPane = createPane(createSessionTab(sessions[0].id));
    panes = [initialPane];
    root = {
      type: "pane",
      paneId: initialPane.id,
    };
  }

  if (!root) {
    return {
      root: null,
      panes: [],
      activePaneId: null,
    };
  }

  const activePaneId = panes.some((pane) => pane.id === current.activePaneId)
    ? current.activePaneId
    : (panes[0]?.id ?? null);

  return {
    root,
    panes,
    activePaneId,
  };
}

export function openSessionInWorkspaceState(
  workspace: WorkspaceState,
  sessionId: string,
  preferredPaneId: string | null,
): WorkspaceState {
  const existing = findSessionTab(workspace, sessionId);
  if (existing) {
    return activatePane(workspace, existing.paneId, existing.tab.id);
  }

  return openTabInWorkspaceState(workspace, createSessionTab(sessionId), preferredPaneId);
}

export function openSourceInWorkspaceState(
  workspace: WorkspaceState,
  path: string | null,
  preferredPaneId: string | null,
  originSessionId: string | null,
): WorkspaceState {
  const normalizedPath = normalizeWorkspacePath(path);
  const nextTab = createSourceTab(normalizedPath, originSessionId);
  if (normalizedPath) {
    const existing = findSourceTab(workspace, normalizedPath);
    return openContextualTabInWorkspaceState(
      workspace,
      nextTab,
      existing,
      preferredPaneId,
      originSessionId,
    );
  }

  return openContextualTabInWorkspaceState(workspace, nextTab, null, preferredPaneId, originSessionId);
}

export function openFilesystemInWorkspaceState(
  workspace: WorkspaceState,
  rootPath: string | null,
  preferredPaneId: string | null,
  originSessionId: string | null,
): WorkspaceState {
  const normalizedRootPath = normalizeWorkspacePath(rootPath);
  if (normalizedRootPath) {
    const existing = findFilesystemTab(workspace, normalizedRootPath);
    if (existing) {
      return activatePane(workspace, existing.paneId, existing.tab.id);
    }
  }

  return openTabInWorkspaceState(
    workspace,
    createFilesystemTab(normalizedRootPath, originSessionId),
    preferredPaneId,
  );
}

export function openGitStatusInWorkspaceState(
  workspace: WorkspaceState,
  workdir: string | null,
  preferredPaneId: string | null,
  originSessionId: string | null,
): WorkspaceState {
  const normalizedWorkdir = normalizeWorkspacePath(workdir);
  if (normalizedWorkdir) {
    const existing = findGitStatusTab(workspace, normalizedWorkdir);
    if (existing) {
      return activatePane(workspace, existing.paneId, existing.tab.id);
    }
  }

  return openTabInWorkspaceState(
    workspace,
    createGitStatusTab(normalizedWorkdir, originSessionId),
    preferredPaneId,
  );
}

export function openControlPanelInWorkspaceState(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
): WorkspaceState {
  const existing = findControlPanelTab(workspace);
  if (existing) {
    return activatePane(workspace, existing.paneId, existing.tab.id);
  }

  return openTabInWorkspaceState(
    workspace,
    createControlPanelTab(originSessionId),
    preferredPaneId,
  );
}

export function ensureControlPanelInWorkspaceState(workspace: WorkspaceState): WorkspaceState {
  if (findControlPanelTab(workspace)) {
    return workspace;
  }

  return openTabInWorkspaceState(
    workspace,
    createControlPanelTab(null),
    findDefaultControlPanelAnchorPaneId(workspace),
  );
}

export function dockControlPanelAtWorkspaceEdge(
  workspace: WorkspaceState,
  side: "left" | "right",
): WorkspaceState {
  const controlPanel = findControlPanelTab(workspace);
  if (!controlPanel || !workspace.root) {
    return workspace;
  }

  const contentRoot = removePaneFromTree(workspace.root, controlPanel.paneId);
  if (!contentRoot) {
    return workspace;
  }

  const controlPanelNode: WorkspaceNode = {
    type: "pane",
    paneId: controlPanel.paneId,
  };

  return {
    ...workspace,
    root: {
      id: crypto.randomUUID(),
      type: "split",
      direction: "row",
      ratio: side === "left" ? 0.24 : 0.76,
      first: side === "left" ? controlPanelNode : contentRoot,
      second: side === "left" ? contentRoot : controlPanelNode,
    },
  };
}

export function openDiffPreviewInWorkspaceState(
  workspace: WorkspaceState,
  tab: {
    changeType: DiffMessage["changeType"];
    diff: string;
    diffMessageId: string;
    filePath: string | null;
    language?: string | null;
    originSessionId: string | null;
    summary: string;
  },
  preferredPaneId: string | null,
): WorkspaceState {
  const nextTab = createDiffPreviewTab(tab);
  const existing = findDiffPreviewTab(workspace, tab.diffMessageId, tab.originSessionId);
  return openContextualTabInWorkspaceState(
    workspace,
    nextTab,
    existing,
    preferredPaneId,
    tab.originSessionId,
  );
}

export function activatePane(
  workspace: WorkspaceState,
  paneId: string,
  tabId?: string | null,
): WorkspaceState {
  const targetPane = workspace.panes.find((pane) => pane.id === paneId) ?? null;
  const targetActiveTabId =
    targetPane &&
    (tabId && targetPane.tabs.some((tab) => tab.id === tabId)
      ? tabId
      : (targetPane.activeTabId ?? targetPane.tabs[0]?.id ?? null));

  if (
    targetPane &&
    workspace.activePaneId === paneId &&
    targetPane.activeTabId === targetActiveTabId
  ) {
    return workspace;
  }

  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      const activeTabId =
        tabId && pane.tabs.some((tab) => tab.id === tabId)
          ? tabId
          : (pane.activeTabId ?? pane.tabs[0]?.id ?? null);

      return syncPaneState({
        ...pane,
        activeTabId,
      });
    }),
    activePaneId: paneId,
  };
}

export function closeWorkspaceTab(
  workspace: WorkspaceState,
  paneId: string,
  tabId: string,
): WorkspaceState {
  const pane = workspace.panes.find((candidate) => candidate.id === paneId);
  if (!pane) {
    return workspace;
  }

  const nextTabs = pane.tabs.filter((candidate) => candidate.id !== tabId);
  if (nextTabs.length === 0) {
    const panes = workspace.panes.filter((candidate) => candidate.id !== paneId);
    const root = removePaneFromTree(workspace.root, paneId);

    return {
      root,
      panes,
      activePaneId:
        panes.some((candidate) => candidate.id === workspace.activePaneId)
          ? workspace.activePaneId
          : (panes[0]?.id ?? null),
    };
  }

  return {
    ...workspace,
    panes: workspace.panes.map((candidate) => {
      if (candidate.id !== paneId) {
        return candidate;
      }

      return syncPaneState({
        ...candidate,
        tabs: nextTabs,
        activeTabId:
          candidate.activeTabId === tabId ? (nextTabs[0]?.id ?? null) : candidate.activeTabId,
      });
    }),
    activePaneId: paneId,
  };
}

export function splitPane(
  workspace: WorkspaceState,
  paneId: string,
  direction: "row" | "column",
): WorkspaceState {
  const pane = workspace.panes.find((candidate) => candidate.id === paneId);
  if (!pane || !workspace.root) {
    return workspace;
  }

  const activeTab = pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? null;
  const tabToMove = pane.tabs.length > 1 ? activeTab : null;
  const newPane = createPane(tabToMove, pane.lastSessionViewMode);
  const panes = workspace.panes.map((candidate) => {
    if (candidate.id !== paneId || !tabToMove) {
      return candidate;
    }

    return syncPaneState({
      ...candidate,
      tabs: candidate.tabs.filter((tab) => tab.id !== tabToMove.id),
      activeTabId:
        candidate.activeTabId === tabToMove.id
          ? (candidate.tabs.find((tab) => tab.id !== tabToMove.id)?.id ?? null)
          : candidate.activeTabId,
    });
  });

  return {
    root: insertPaneAdjacent(workspace.root, paneId, direction, newPane.id, false),
    panes: [...panes, newPane],
    activePaneId: newPane.id,
  };
}

export function placeDraggedTab(
  workspace: WorkspaceState,
  sourcePaneId: string,
  tabId: string,
  targetPaneId: string,
  placement: TabDropPlacement,
  tabIndex?: number,
): WorkspaceState {
  const sourcePane = workspace.panes.find((pane) => pane.id === sourcePaneId);
  const targetPane = workspace.panes.find((pane) => pane.id === targetPaneId);
  const draggedTab = sourcePane?.tabs.find((tab) => tab.id === tabId);
  if (!sourcePane || !targetPane || !draggedTab) {
    return workspace;
  }

  if (!isAllowedControlPanelPlacement(targetPane, draggedTab, placement)) {
    return workspace;
  }

  if (placement === "tabs") {
    const requestedTabIndex = tabIndex ?? targetPane.tabs.length;
    if (sourcePaneId === targetPaneId) {
      const sourceTabIndex = sourcePane.tabs.findIndex((tab) => tab.id === tabId);
      const adjustedTabIndex =
        sourceTabIndex >= 0 && requestedTabIndex > sourceTabIndex
          ? requestedTabIndex - 1
          : requestedTabIndex;

      return addWorkspaceTabToPane(workspace, targetPaneId, draggedTab, adjustedTabIndex);
    }

    const withoutSource = closeWorkspaceTab(workspace, sourcePaneId, tabId);
    return addWorkspaceTabToPane(withoutSource, targetPaneId, draggedTab, requestedTabIndex);
  }

  if (sourcePaneId === targetPaneId && sourcePane.tabs.length <= 1) {
    return workspace;
  }

  const withoutSource = closeWorkspaceTab(workspace, sourcePaneId, tabId);
  if (!withoutSource.root || !withoutSource.panes.some((pane) => pane.id === targetPaneId)) {
    return workspace;
  }

  const newPane = createPane(draggedTab, targetPane.lastSessionViewMode);
  const direction = placement === "left" || placement === "right" ? "row" : "column";
  const placeBefore = placement === "left" || placement === "top";

  return {
    root: insertPaneAdjacent(withoutSource.root, targetPaneId, direction, newPane.id, placeBefore),
    panes: [...withoutSource.panes, newPane],
    activePaneId: newPane.id,
  };
}

export function placeExternalTab(
  workspace: WorkspaceState,
  tab: WorkspaceTab,
  targetPaneId: string,
  placement: TabDropPlacement,
  tabIndex?: number,
): WorkspaceState {
  const transferredTab = cloneWorkspaceTab(tab);

  if (placement === "tabs") {
    return openTabInWorkspaceState(workspace, transferredTab, targetPaneId, tabIndex);
  }

  const targetPane = workspace.panes.find((pane) => pane.id === targetPaneId);
  if (!targetPane || !workspace.root) {
    return openTabInWorkspaceState(workspace, transferredTab, targetPaneId, tabIndex);
  }

  if (!isAllowedControlPanelPlacement(targetPane, transferredTab, placement)) {
    return workspace;
  }

  const newPane = createPane(transferredTab, targetPane.lastSessionViewMode);
  const direction = placement === "left" || placement === "right" ? "row" : "column";
  const placeBefore = placement === "left" || placement === "top";

  return {
    root: insertPaneAdjacent(workspace.root, targetPaneId, direction, newPane.id, placeBefore),
    panes: [...workspace.panes, newPane],
    activePaneId: newPane.id,
  };
}

export function updateSplitRatio(
  workspace: WorkspaceState,
  splitId: string,
  ratio: number,
): WorkspaceState {
  if (!workspace.root) {
    return workspace;
  }

  return {
    ...workspace,
    root: updateSplitRatioInNode(workspace.root, splitId, ratio),
  };
}

export function createPane(
  initialTab?: WorkspaceTab | null,
  sessionViewMode: SessionPaneViewMode = "session",
): WorkspacePane {
  return syncPaneState({
    id: crypto.randomUUID(),
    tabs: initialTab ? [initialTab] : [],
    activeTabId: initialTab?.id ?? null,
    activeSessionId: null,
    viewMode: sessionViewMode,
    lastSessionViewMode: sessionViewMode,
    sourcePath: null,
  });
}

export function setPaneViewMode(
  workspace: WorkspaceState,
  paneId: string,
  viewMode: SessionPaneViewMode,
): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId || !isSessionTabActive(pane)) {
        return pane;
      }

      return syncPaneState({
        ...pane,
        viewMode,
        lastSessionViewMode: viewMode,
      });
    }),
  };
}

export function setPaneSourcePath(
  workspace: WorkspaceState,
  paneId: string,
  sourcePath: string,
): WorkspaceState {
  const nextPath = normalizeWorkspacePath(sourcePath);
  const currentPane = workspace.panes.find((pane) => pane.id === paneId);
  const activeSourceTabId =
    currentPane?.tabs.find(
      (tab): tab is WorkspaceSourceTab => tab.id === currentPane.activeTabId && tab.kind === "source",
    )?.id ?? null;
  const existing = nextPath ? findSourceTab(workspace, nextPath) : null;
  if (existing && existing.tab.id !== activeSourceTabId) {
    return activatePane(workspace, existing.paneId, existing.tab.id);
  }

  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      const activeTab = pane.tabs.find((tab) => tab.id === pane.activeTabId);
      if (!activeTab || activeTab.kind !== "source") {
        return pane;
      }

      return syncPaneState({
        ...pane,
        tabs: pane.tabs.map((tab) =>
          tab.id === activeTab.id
            ? {
                ...tab,
                path: nextPath,
                originSessionId: activeTab.originSessionId ?? pane.activeSessionId ?? null,
              }
            : tab,
        ),
      });
    }),
  };
}

export function addWorkspaceTabToPane(
  workspace: WorkspaceState,
  paneId: string,
  tab: WorkspaceTab,
  tabIndex?: number,
): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      return syncPaneState({
        ...pane,
        tabs: insertTabAtIndex(pane.tabs, tab, tabIndex ?? pane.tabs.length),
        activeTabId: tab.id,
      });
    }),
    activePaneId: paneId,
  };
}

export function getSplitRatio(node: WorkspaceNode | null, splitId: string): number | null {
  if (!node || node.type === "pane") {
    return null;
  }

  if (node.id === splitId) {
    return node.ratio;
  }

  return getSplitRatio(node.first, splitId) ?? getSplitRatio(node.second, splitId);
}

function openTabInWorkspaceState(
  workspace: WorkspaceState,
  tab: WorkspaceTab,
  preferredPaneId: string | null,
  tabIndex?: number,
): WorkspaceState {
  const targetPaneId = workspace.panes.some((pane) => pane.id === preferredPaneId)
    ? preferredPaneId
    : (workspace.activePaneId ?? workspace.panes[0]?.id ?? null);

  if (!targetPaneId) {
    const pane = createPane(tab);
    return {
      root: {
        type: "pane",
        paneId: pane.id,
      },
      panes: [pane],
      activePaneId: pane.id,
    };
  }

  const targetPane = workspace.panes.find((pane) => pane.id === targetPaneId) ?? null;
  if (targetPane && tab.kind !== "controlPanel" && paneContainsControlPanel(targetPane)) {
    const alternatePaneId = findNonControlPanelPaneId(workspace, targetPane.id);
    if (alternatePaneId) {
      return addWorkspaceTabToPane(workspace, alternatePaneId, tab, tabIndex);
    }

    return openTabInAdjacentPane(workspace, targetPane.id, tab, "row", false);
  }

  if (targetPane && tab.kind === "controlPanel" && !paneContainsControlPanel(targetPane)) {
    return openTabInAdjacentPane(workspace, targetPane.id, tab, "row", true);
  }

  return addWorkspaceTabToPane(workspace, targetPaneId, tab, tabIndex);
}

function openTabInAdjacentPane(
  workspace: WorkspaceState,
  paneId: string,
  tab: WorkspaceTab,
  direction: "row" | "column",
  placeBefore: boolean,
): WorkspaceState {
  const referencePane = workspace.panes.find((pane) => pane.id === paneId);
  if (!referencePane || !workspace.root) {
    return openTabInWorkspaceState(workspace, tab, paneId);
  }

  const newPane = createPane(tab, referencePane.lastSessionViewMode);
  return {
    root: insertPaneAdjacent(workspace.root, paneId, direction, newPane.id, placeBefore),
    panes: [...workspace.panes, newPane],
    activePaneId: newPane.id,
  };
}

function openContextualTabInWorkspaceState<T extends WorkspaceTab>(
  workspace: WorkspaceState,
  tab: T,
  existing: { paneId: string; tab: T } | null,
  preferredPaneId: string | null,
  originSessionId: string | null,
): WorkspaceState {
  if (existing) {
    return activatePane(workspace, existing.paneId, existing.tab.id);
  }

  const targetPaneId = findContextualTargetPaneId(
    workspace,
    preferredPaneId,
    originSessionId,
    tab.kind,
  );
  if (targetPaneId) {
    return openTabInWorkspaceState(workspace, tab, targetPaneId);
  }

  if (shouldOpenTabInAdjacentPane(workspace, preferredPaneId, tab.kind)) {
    return openTabInAdjacentPane(workspace, preferredPaneId!, tab, "row", false);
  }

  return openTabInWorkspaceState(workspace, tab, preferredPaneId);
}

function syncPaneState(pane: WorkspacePane): WorkspacePane {
  const activeTab = pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? pane.tabs[0] ?? null;
  if (!activeTab) {
    return {
      ...pane,
      activeTabId: null,
      activeSessionId: null,
      viewMode: pane.lastSessionViewMode,
      sourcePath: null,
    };
  }

  if (activeTab.kind === "session") {
    const viewMode = pane.viewMode === "source" ? pane.lastSessionViewMode : pane.viewMode;
    const nextSessionViewMode = normalizeSessionViewMode(viewMode, pane.lastSessionViewMode);
    return {
      ...pane,
      activeTabId: activeTab.id,
      activeSessionId: activeTab.sessionId,
      viewMode: nextSessionViewMode,
      lastSessionViewMode: nextSessionViewMode,
      sourcePath: null,
    };
  }

  if (activeTab.kind === "source") {
    return {
      ...pane,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(activeTab.originSessionId, pane.activeSessionId, pane.tabs),
      viewMode: "source",
      sourcePath: activeTab.path,
    };
  }

  if (activeTab.kind === "filesystem") {
    return {
      ...pane,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(activeTab.originSessionId, pane.activeSessionId, pane.tabs),
      viewMode: "filesystem",
      sourcePath: null,
    };
  }

  if (activeTab.kind === "controlPanel") {
    return {
      ...pane,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(activeTab.originSessionId, pane.activeSessionId, pane.tabs),
      viewMode: "controlPanel",
      sourcePath: null,
    };
  }

  return {
    ...pane,
    activeTabId: activeTab.id,
    activeSessionId: resolveOriginSessionId(activeTab.originSessionId, pane.activeSessionId, pane.tabs),
    viewMode: activeTab.kind === "gitStatus" ? "gitStatus" : "diffPreview",
    sourcePath: null,
  };
}

function normalizeSessionViewMode(
  viewMode: PaneViewMode,
  fallback: SessionPaneViewMode,
): SessionPaneViewMode {
  return isSessionPaneViewMode(viewMode) ? viewMode : fallback;
}

function firstSessionTabId(tabs: WorkspaceTab[]): string | null {
  return tabs.find((tab): tab is WorkspaceSessionTab => tab.kind === "session")?.sessionId ?? null;
}

function isSessionTabActive(pane: WorkspacePane) {
  return pane.tabs.some((tab) => tab.id === pane.activeTabId && tab.kind === "session");
}

function findSessionTab(workspace: WorkspaceState, sessionId: string) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceSessionTab =>
        candidate.kind === "session" && candidate.sessionId === sessionId,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findSourceTab(workspace: WorkspaceState, path: string) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceSourceTab =>
        candidate.kind === "source" && candidate.path === path,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findFilesystemTab(workspace: WorkspaceState, rootPath: string) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceFilesystemTab =>
        candidate.kind === "filesystem" && candidate.rootPath === rootPath,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findGitStatusTab(workspace: WorkspaceState, workdir: string) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceGitStatusTab =>
        candidate.kind === "gitStatus" && candidate.workdir === workdir,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findControlPanelTab(workspace: WorkspaceState) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceControlPanelTab => candidate.kind === "controlPanel",
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findDefaultControlPanelAnchorPaneId(workspace: WorkspaceState) {
  return (
    findNonControlPanelPaneId(workspace, null) ??
    workspace.activePaneId ??
    workspace.panes[0]?.id ??
    null
  );
}

function findNonControlPanelPaneId(workspace: WorkspaceState, excludePaneId: string | null) {
  const activePane =
    workspace.activePaneId && workspace.activePaneId !== excludePaneId
      ? workspace.panes.find((pane) => pane.id === workspace.activePaneId) ?? null
      : null;
  if (activePane && !paneContainsControlPanel(activePane)) {
    return activePane.id;
  }

  return (
    workspace.panes.find(
      (pane) => pane.id !== excludePaneId && !paneContainsControlPanel(pane),
    )?.id ?? null
  );
}

function paneContainsControlPanel(pane: WorkspacePane) {
  return pane.tabs.some((tab) => tab.kind === "controlPanel");
}

function isAllowedControlPanelPlacement(
  targetPane: WorkspacePane,
  tab: WorkspaceTab,
  placement: TabDropPlacement,
) {
  if (tab.kind === "controlPanel" || paneContainsControlPanel(targetPane)) {
    return placement === "left" || placement === "right";
  }

  return true;
}

function findDiffPreviewTab(
  workspace: WorkspaceState,
  diffMessageId: string,
  originSessionId: string | null,
) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceDiffPreviewTab =>
        candidate.kind === "diffPreview" &&
        candidate.diffMessageId === diffMessageId &&
        candidate.originSessionId === originSessionId,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function cloneWorkspaceTab(tab: WorkspaceTab): WorkspaceTab {
  return {
    ...tab,
    id: crypto.randomUUID(),
  };
}

function insertTabAtIndex(tabs: WorkspaceTab[], tab: WorkspaceTab, tabIndex: number): WorkspaceTab[] {
  const nextTabs = tabs.filter((candidate) => candidate.id !== tab.id);
  const nextTabIndex = clampIndex(tabIndex, 0, nextTabs.length);
  nextTabs.splice(nextTabIndex, 0, tab);
  return nextTabs;
}

function shouldOpenTabInAdjacentPane(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  tabKind: WorkspaceTab["kind"],
) {
  if (!preferredPaneId) {
    return false;
  }

  const preferredPane = workspace.panes.find((pane) => pane.id === preferredPaneId);
  if (!preferredPane) {
    return false;
  }

  const activeTab = getActiveTab(preferredPane);
  return activeTab !== null && activeTab.kind !== tabKind;
}

function findContextualTargetPaneId(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
  tabKind: WorkspaceTab["kind"],
) {
  const preferredPane = preferredPaneId
    ? workspace.panes.find((pane) => pane.id === preferredPaneId) ?? null
    : null;
  const preferredActiveTab = preferredPane ? getActiveTab(preferredPane) : null;

  if (preferredActiveTab?.kind === tabKind) {
    return preferredPane!.id;
  }

  if (originSessionId) {
    const relatedPane = workspace.panes.find(
      (pane) =>
        pane.id !== preferredPaneId &&
        pane.activeSessionId === originSessionId &&
        pane.tabs.some((tab) => tab.kind === tabKind),
    );
    if (relatedPane) {
      return relatedPane.id;
    }
  }

  return null;
}

function getActiveTab(pane: WorkspacePane) {
  return pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? pane.tabs[0] ?? null;
}

function normalizeWorkspacePath(path: string | null | undefined) {
  const trimmed = path?.trim();
  return trimmed ? trimmed : null;
}

function resolveOriginSessionId(
  originSessionId: string | null,
  activeSessionId: string | null,
  tabs: WorkspaceTab[],
) {
  return originSessionId ?? activeSessionId ?? firstSessionTabId(tabs);
}

function isSessionPaneViewMode(viewMode: PaneViewMode): viewMode is SessionPaneViewMode {
  return (
    viewMode === "session" ||
    viewMode === "prompt" ||
    viewMode === "commands" ||
    viewMode === "diffs"
  );
}

function clampIndex(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

function pruneWorkspaceNode(node: WorkspaceNode | null, availablePaneIds: Set<string>): WorkspaceNode | null {
  if (!node) {
    return null;
  }

  if (node.type === "pane") {
    return availablePaneIds.has(node.paneId) ? node : null;
  }

  const first = pruneWorkspaceNode(node.first, availablePaneIds);
  const second = pruneWorkspaceNode(node.second, availablePaneIds);
  if (!first && !second) {
    return null;
  }
  if (!first) {
    return second;
  }
  if (!second) {
    return first;
  }

  return {
    ...node,
    first,
    second,
  };
}

function removePaneFromTree(node: WorkspaceNode | null, paneId: string): WorkspaceNode | null {
  if (!node) {
    return null;
  }

  if (node.type === "pane") {
    return node.paneId === paneId ? null : node;
  }

  const first = removePaneFromTree(node.first, paneId);
  const second = removePaneFromTree(node.second, paneId);
  if (!first && !second) {
    return null;
  }
  if (!first) {
    return second;
  }
  if (!second) {
    return first;
  }

  return {
    ...node,
    first,
    second,
  };
}

function insertPaneAdjacent(
  node: WorkspaceNode,
  paneId: string,
  direction: "row" | "column",
  newPaneId: string,
  placeBefore: boolean,
): WorkspaceNode {
  if (node.type === "pane") {
    if (node.paneId !== paneId) {
      return node;
    }

    const insertedPane: WorkspaceNode = {
      type: "pane",
      paneId: newPaneId,
    };

    return {
      id: crypto.randomUUID(),
      type: "split",
      direction,
      ratio: 0.5,
      first: placeBefore ? insertedPane : node,
      second: placeBefore ? node : insertedPane,
    };
  }

  return {
    ...node,
    first: insertPaneAdjacent(node.first, paneId, direction, newPaneId, placeBefore),
    second: insertPaneAdjacent(node.second, paneId, direction, newPaneId, placeBefore),
  };
}

function updateSplitRatioInNode(node: WorkspaceNode, splitId: string, ratio: number): WorkspaceNode {
  if (node.type === "pane") {
    return node;
  }

  if (node.id === splitId) {
    return {
      ...node,
      ratio,
    };
  }

  return {
    ...node,
    first: updateSplitRatioInNode(node.first, splitId, ratio),
    second: updateSplitRatioInNode(node.second, splitId, ratio),
  };
}
