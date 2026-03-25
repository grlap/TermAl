import type {
  PaneViewMode,
  SessionPaneViewMode,
  WorkspaceNode,
  WorkspacePane,
  WorkspaceState,
  WorkspaceTab,
} from "./workspace";

export const WORKSPACE_LAYOUT_STORAGE_KEY = "termal-workspace-layout";

export type ControlPanelSide = "left" | "right";

export type StoredWorkspaceLayout = {
  controlPanelSide: ControlPanelSide;
  workspace: WorkspaceState;
};

const SESSION_PANE_VIEW_MODES: readonly SessionPaneViewMode[] = [
  "session",
  "prompt",
  "commands",
  "diffs",
];
const PANE_VIEW_MODES: readonly PaneViewMode[] = [
  ...SESSION_PANE_VIEW_MODES,
  "canvas",
  "controlPanel",
  "sessionList",
  "projectList",
  "source",
  "filesystem",
  "gitStatus",
  "instructionDebugger",
  "diffPreview",
];
const DIFF_CHANGE_TYPES = ["edit", "create"] as const;

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

function isWorkspaceTab(value: unknown): value is WorkspaceTab {
  if (!isRecord(value) || !isString(value.id) || !isString(value.kind)) {
    return false;
  }

  switch (value.kind) {
    case "session":
      return isString(value.sessionId);
    case "source":
      return isNullableString(value.path) && isNullableString(value.originSessionId) && isOptionalNullableString(value.originProjectId);
    case "filesystem":
      return isNullableString(value.rootPath) && isNullableString(value.originSessionId) && isOptionalNullableString(value.originProjectId);
    case "gitStatus":
      return isNullableString(value.workdir) && isNullableString(value.originSessionId) && isOptionalNullableString(value.originProjectId);
    case "controlPanel":
      return isNullableString(value.originSessionId) && isOptionalNullableString(value.originProjectId);
    case "canvas":
      return (
        Array.isArray(value.cards) &&
        value.cards.every((card) => isWorkspaceCanvasCard(card)) &&
        isOptionalWorkspaceCanvasZoom(value.zoom) &&
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId)
      );
    case "sessionList":
      return isNullableString(value.originSessionId) && isOptionalNullableString(value.originProjectId);
    case "projectList":
      return isNullableString(value.originSessionId) && isOptionalNullableString(value.originProjectId);
    case "instructionDebugger":
      return (
        isNullableString(value.workdir) &&
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId)
      );
    case "diffPreview":
      return (
        isString(value.diff) &&
        isOptionalNullableString(value.changeSetId) &&
        isString(value.diffMessageId) &&
        isNullableString(value.filePath) &&
        isOptionalNullableString(value.language) &&
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId) &&
        isString(value.summary) &&
        isDiffChangeType(value.changeType)
      );
    default:
      return false;
  }
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

function isPaneViewMode(value: unknown): value is PaneViewMode {
  return PANE_VIEW_MODES.includes(value as PaneViewMode);
}

function isSessionPaneViewMode(value: unknown): value is SessionPaneViewMode {
  return SESSION_PANE_VIEW_MODES.includes(value as SessionPaneViewMode);
}

function isWorkspaceCanvasCard(value: unknown) {
  return (
    isRecord(value) &&
    isString(value.sessionId) &&
    typeof value.x === "number" &&
    Number.isFinite(value.x) &&
    typeof value.y === "number" &&
    Number.isFinite(value.y)
  );
}

function isOptionalWorkspaceCanvasZoom(value: unknown) {
  return typeof value === "undefined" || isWorkspaceCanvasZoom(value);
}

function isWorkspaceCanvasZoom(value: unknown) {
  return typeof value === "number" && Number.isFinite(value) && value > 0;
}

function isDiffChangeType(value: unknown): value is (typeof DIFF_CHANGE_TYPES)[number] {
  return DIFF_CHANGE_TYPES.includes(value as (typeof DIFF_CHANGE_TYPES)[number]);
}

function isValidSplitRatio(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 && value < 1;
}

function isOptionalNullableString(value: unknown): value is string | null | undefined {
  return typeof value === "undefined" || isNullableString(value);
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
