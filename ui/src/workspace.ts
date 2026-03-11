import type { Session } from "./types";

export type WorkspacePane = {
  id: string;
  sessionIds: string[];
  activeSessionId: string | null;
  viewMode: PaneViewMode;
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
export type PaneViewMode = "session" | "prompt" | "commands" | "diffs" | "source";

export function reconcileWorkspaceState(current: WorkspaceState, sessions: Session[]): WorkspaceState {
  const availableSessionIds = new Set(sessions.map((session) => session.id));
  let panes = current.panes.map((pane) => {
    const sessionIds = pane.sessionIds.filter((sessionId) => availableSessionIds.has(sessionId));
    const activeSessionId = sessionIds.includes(pane.activeSessionId ?? "")
      ? pane.activeSessionId
      : (sessionIds[0] ?? null);

    return {
      ...pane,
      sessionIds,
      activeSessionId,
    };
  });

  let root = pruneWorkspaceNode(current.root, new Set(panes.map((pane) => pane.id)));

  if (!root && panes.length > 0) {
    root = {
      type: "pane",
      paneId: panes[0].id,
    };
  }

  if (!root && sessions.length > 0) {
    const initialPane = createPane(sessions[0].id);
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
  const existingPane = workspace.panes.find((pane) => pane.sessionIds.includes(sessionId));
  if (existingPane) {
    return activatePane(workspace, existingPane.id, sessionId);
  }

  const targetPaneId = workspace.panes.some((pane) => pane.id === preferredPaneId)
    ? preferredPaneId
    : (workspace.activePaneId ?? workspace.panes[0]?.id ?? null);

  if (!targetPaneId) {
    const pane = createPane(sessionId);
    return {
      root: {
        type: "pane",
        paneId: pane.id,
      },
      panes: [pane],
      activePaneId: pane.id,
    };
  }

  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== targetPaneId) {
        return pane;
      }

      return {
        ...pane,
        sessionIds: pane.sessionIds.includes(sessionId) ? pane.sessionIds : [...pane.sessionIds, sessionId],
        activeSessionId: sessionId,
      };
    }),
    activePaneId: targetPaneId,
  };
}

export function activatePane(
  workspace: WorkspaceState,
  paneId: string,
  sessionId?: string | null,
): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      return {
        ...pane,
        activeSessionId:
          sessionId && pane.sessionIds.includes(sessionId)
            ? sessionId
            : (pane.activeSessionId ?? pane.sessionIds[0] ?? null),
      };
    }),
    activePaneId: paneId,
  };
}

export function closeSessionTab(
  workspace: WorkspaceState,
  paneId: string,
  sessionId: string,
): WorkspaceState {
  const pane = workspace.panes.find((candidate) => candidate.id === paneId);
  if (!pane) {
    return workspace;
  }

  const nextSessionIds = pane.sessionIds.filter((candidate) => candidate !== sessionId);
  if (nextSessionIds.length === 0) {
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

      return {
        ...candidate,
        sessionIds: nextSessionIds,
        activeSessionId:
          candidate.activeSessionId === sessionId ? (nextSessionIds[0] ?? null) : candidate.activeSessionId,
      };
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

  const sessionToMove = pane.sessionIds.length > 1 ? (pane.activeSessionId ?? null) : null;
  const newPane = createPane(sessionToMove ?? undefined, pane.viewMode, pane.sourcePath);
  const panes = workspace.panes.map((candidate) => {
    if (candidate.id !== paneId) {
      return candidate;
    }

    if (!sessionToMove) {
      return candidate;
    }

    const nextSessionIds = candidate.sessionIds.filter((sessionId) => sessionId !== sessionToMove);
    return {
      ...candidate,
      sessionIds: nextSessionIds,
      activeSessionId: nextSessionIds[0] ?? null,
    };
  });

  return {
    root: insertPaneAdjacent(workspace.root, paneId, direction, newPane.id, false),
    panes: [...panes, newPane],
    activePaneId: newPane.id,
  };
}

export function placeDraggedSession(
  workspace: WorkspaceState,
  sourcePaneId: string,
  sessionId: string,
  targetPaneId: string,
  placement: TabDropPlacement,
  tabIndex?: number,
): WorkspaceState {
  const sourcePane = workspace.panes.find((pane) => pane.id === sourcePaneId);
  const targetPane = workspace.panes.find((pane) => pane.id === targetPaneId);
  if (!sourcePane || !targetPane || !sourcePane.sessionIds.includes(sessionId)) {
    return workspace;
  }

  if (placement === "tabs") {
    const requestedTabIndex = tabIndex ?? targetPane.sessionIds.length;
    if (sourcePaneId === targetPaneId) {
      const sourceTabIndex = sourcePane.sessionIds.indexOf(sessionId);
      const adjustedTabIndex =
        sourceTabIndex >= 0 && requestedTabIndex > sourceTabIndex
          ? requestedTabIndex - 1
          : requestedTabIndex;

      return addSessionToPane(workspace, targetPaneId, sessionId, adjustedTabIndex);
    }

    const withoutSource = closeSessionTab(workspace, sourcePaneId, sessionId);
    return addSessionToPane(withoutSource, targetPaneId, sessionId, requestedTabIndex);
  }

  if (sourcePaneId === targetPaneId && sourcePane.sessionIds.length <= 1) {
    return workspace;
  }

  const withoutSource = closeSessionTab(workspace, sourcePaneId, sessionId);
  if (!withoutSource.root || !withoutSource.panes.some((pane) => pane.id === targetPaneId)) {
    return workspace;
  }

  const newPane = createPane(sessionId, targetPane.viewMode, targetPane.sourcePath);
  const direction = placement === "left" || placement === "right" ? "row" : "column";
  const placeBefore = placement === "left" || placement === "top";

  return {
    root: insertPaneAdjacent(withoutSource.root, targetPaneId, direction, newPane.id, placeBefore),
    panes: [...withoutSource.panes, newPane],
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
  sessionId?: string,
  viewMode: PaneViewMode = "session",
  sourcePath: string | null = null,
): WorkspacePane {
  return {
    id: crypto.randomUUID(),
    sessionIds: sessionId ? [sessionId] : [],
    activeSessionId: sessionId ?? null,
    viewMode,
    sourcePath,
  };
}

export function setPaneViewMode(
  workspace: WorkspaceState,
  paneId: string,
  viewMode: PaneViewMode,
): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      return {
        ...pane,
        viewMode,
      };
    }),
  };
}

export function setPaneSourcePath(
  workspace: WorkspaceState,
  paneId: string,
  sourcePath: string,
): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      return {
        ...pane,
        sourcePath,
      };
    }),
  };
}

export function addSessionToPane(
  workspace: WorkspaceState,
  paneId: string,
  sessionId: string,
  tabIndex?: number,
): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      return {
        ...pane,
        sessionIds: insertSessionIdAtIndex(pane.sessionIds, sessionId, tabIndex ?? pane.sessionIds.length),
        activeSessionId: sessionId,
      };
    }),
    activePaneId: paneId,
  };
}

function insertSessionIdAtIndex(sessionIds: string[], sessionId: string, tabIndex: number): string[] {
  const nextSessionIds = sessionIds.filter((candidate) => candidate !== sessionId);
  const nextTabIndex = clampIndex(tabIndex, 0, nextSessionIds.length);
  nextSessionIds.splice(nextTabIndex, 0, sessionId);
  return nextSessionIds;
}

function clampIndex(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
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
