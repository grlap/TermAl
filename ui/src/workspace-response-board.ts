// Owns Response Board-specific workspace camera normalization and tab updates.
// Deliberately does not own generic pane routing, activation, or tree mutation.
// Split from workspace.ts.

import { normalizeWorkspaceIdentifier } from "./workspace-normalize";
import type {
  WorkspacePane,
  WorkspaceResponseBoardTab,
  WorkspaceResponseBoardView,
  WorkspaceState,
} from "./workspace-types";

export function normalizeResponseBoardViews(
  value: Record<string, WorkspaceResponseBoardView> | undefined,
): Record<string, WorkspaceResponseBoardView> {
  const normalized: Record<string, WorkspaceResponseBoardView> = {};
  if (!value || typeof value !== "object") {
    return normalized;
  }
  for (const [tabId, view] of Object.entries(value)) {
    const normalizedTabId = normalizeWorkspaceIdentifier(tabId);
    if (
      normalizedTabId &&
      view &&
      Number.isFinite(view.panX) &&
      Number.isFinite(view.panY) &&
      Number.isFinite(view.zoom)
    ) {
      normalized[normalizedTabId] = {
        panX: view.panX,
        panY: view.panY,
        zoom: Math.min(2, Math.max(0.25, view.zoom)),
      };
    }
  }
  return normalized;
}

function updateResponseBoardWorkspaceTab(
  workspace: WorkspaceState,
  workspaceTabId: string,
  update: (tab: WorkspaceResponseBoardTab) => WorkspaceResponseBoardTab,
  syncPaneState: (pane: WorkspacePane) => WorkspacePane,
): WorkspaceState {
  let hasChanged = false;
  const panes = workspace.panes.map((pane) => {
    const tabIndex = pane.tabs.findIndex(
      (tab) => tab.id === workspaceTabId && tab.kind === "responseBoard",
    );
    if (tabIndex < 0) {
      return pane;
    }
    const current = pane.tabs[tabIndex];
    if (current.kind !== "responseBoard") {
      return pane;
    }
    const next = update(current);
    if (next === current) {
      return pane;
    }
    hasChanged = true;
    const tabs = [...pane.tabs];
    tabs[tabIndex] = next;
    return syncPaneState({ ...pane, tabs });
  });
  return hasChanged ? { ...workspace, panes } : workspace;
}

export function setResponseBoardWorkspaceStateInPanes(
  workspace: WorkspaceState,
  workspaceTabId: string,
  activeBoardTabId: string,
  view: WorkspaceResponseBoardView,
  knownBoardTabIds: readonly string[] | undefined,
  syncPaneState: (pane: WorkspacePane) => WorkspacePane,
): WorkspaceState {
  const normalizedActiveTabId = normalizeWorkspaceIdentifier(activeBoardTabId);
  if (!normalizedActiveTabId) {
    return workspace;
  }
  return updateResponseBoardWorkspaceTab(
    workspace,
    workspaceTabId,
    (tab) => {
      const normalizedView = normalizeResponseBoardViews({
        [normalizedActiveTabId]: view,
      })[normalizedActiveTabId];
      if (!normalizedView) {
        return tab;
      }
      const previousViews = normalizeResponseBoardViews(tab.boardViews);
      const knownBoardTabIdSet = knownBoardTabIds
        ? new Set(
            knownBoardTabIds
              .map(normalizeWorkspaceIdentifier)
              .filter((tabId): tabId is string => !!tabId),
          )
        : null;
      const retainedViews = knownBoardTabIdSet
        ? Object.fromEntries(
            Object.entries(previousViews).filter(([tabId]) =>
              knownBoardTabIdSet.has(tabId),
            ),
          )
        : previousViews;
      const nextViews = {
        ...retainedViews,
        [normalizedActiveTabId]: normalizedView,
      };
      const previous = previousViews[normalizedActiveTabId];
      const previousViewIds = Object.keys(previousViews);
      const nextViewIds = Object.keys(nextViews);
      if (
        tab.activeBoardTabId === normalizedActiveTabId &&
        previous?.panX === normalizedView.panX &&
        previous.panY === normalizedView.panY &&
        previous.zoom === normalizedView.zoom &&
        previousViewIds.length === nextViewIds.length &&
        previousViewIds.every((tabId) => {
          const before = previousViews[tabId];
          const after = nextViews[tabId];
          return (
            !!before &&
            !!after &&
            before.panX === after.panX &&
            before.panY === after.panY &&
            before.zoom === after.zoom
          );
        })
      ) {
        return tab;
      }
      return {
        ...tab,
        activeBoardTabId: normalizedActiveTabId,
        boardViews: nextViews,
      };
    },
    syncPaneState,
  );
}
