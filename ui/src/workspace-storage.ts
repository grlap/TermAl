import type {
  WorkspaceNode,
  WorkspacePane,
  WorkspaceState,
} from "./workspace";
import {
  isPaneViewMode,
  isSessionPaneViewMode,
  isWorkspaceTab,
} from "./workspace-tab-validation";

export const WORKSPACE_LAYOUT_STORAGE_KEY = "termal-workspace-layout";

export type ControlPanelSide = "left" | "right";

export type StoredWorkspaceLayout = {
  controlPanelSide: ControlPanelSide;
  workspace: WorkspaceState;
};

export function getStoredWorkspaceLayout(): StoredWorkspaceLayout | null {
  if (typeof window === "undefined") {
    return null;
  }

  const raw = window.localStorage.getItem(WORKSPACE_LAYOUT_STORAGE_KEY);
  if (!raw) {
    return null;
  }

  try {
    const parsed = JSON.parse(raw);
    return isStoredWorkspaceLayout(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

export function persistWorkspaceLayout(layout: StoredWorkspaceLayout) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(WORKSPACE_LAYOUT_STORAGE_KEY, JSON.stringify(layout));
}

function isStoredWorkspaceLayout(value: unknown): value is StoredWorkspaceLayout {
  if (!isRecord(value)) {
    return false;
  }

  return isControlPanelSide(value.controlPanelSide) && isWorkspaceState(value.workspace);
}

function isControlPanelSide(value: unknown): value is ControlPanelSide {
  return value === "left" || value === "right";
}

function isWorkspaceState(value: unknown): value is WorkspaceState {
  if (!isRecord(value) || !Array.isArray(value.panes) || !isNullableString(value.activePaneId)) {
    return false;
  }

  const panes = value.panes;
  if (!panes.every((pane) => isWorkspacePane(pane))) {
    return false;
  }

  const paneIds = new Set(panes.map((pane) => pane.id));
  if (value.activePaneId !== null && !paneIds.has(value.activePaneId)) {
    return false;
  }

  if (value.root === null) {
    return panes.length === 0 && value.activePaneId === null;
  }

  return isWorkspaceNode(value.root, paneIds);
}

function isWorkspacePane(value: unknown): value is WorkspacePane {
  if (!isRecord(value) || !isString(value.id) || !Array.isArray(value.tabs)) {
    return false;
  }

  const tabs = value.tabs;
  if (!tabs.every((tab) => isWorkspaceTab(tab))) {
    return false;
  }

  const tabIds = new Set(tabs.map((tab) => tab.id));
  return (
    isNullableString(value.activeTabId) &&
    (value.activeTabId === null || tabIds.has(value.activeTabId)) &&
    isNullableString(value.activeSessionId) &&
    isPaneViewMode(value.viewMode) &&
    isSessionPaneViewMode(value.lastSessionViewMode) &&
    isNullableString(value.sourcePath)
  );
}

function isWorkspaceNode(value: unknown, paneIds: ReadonlySet<string>): value is WorkspaceNode {
  if (!isRecord(value) || !isString(value.type)) {
    return false;
  }

  if (value.type === "pane") {
    return isString(value.paneId) && paneIds.has(value.paneId);
  }

  if (value.type === "split") {
    return (
      isString(value.id) &&
      (value.direction === "row" || value.direction === "column") &&
      isValidSplitRatio(value.ratio) &&
      isWorkspaceNode(value.first, paneIds) &&
      isWorkspaceNode(value.second, paneIds)
    );
  }

  return false;
}

function isValidSplitRatio(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 && value < 1;
}

function isNullableString(value: unknown): value is string | null {
  return value === null || isString(value);
}

function isString(value: unknown): value is string {
  return typeof value === "string";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
