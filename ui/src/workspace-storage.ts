import type { MarkdownStyleId, MarkdownThemeId, StyleId, ThemeId } from "./themes";
import {
  isMarkdownStyleId,
  isMarkdownThemeId,
  isStyleId,
  isThemeId,
} from "./themes";
import {
  normalizeWorkspaceStatePaths,
  stripDiffPreviewDocumentContentFromWorkspaceState,
  stripLoadingGitDiffPreviewTabsFromWorkspaceState,
  type WorkspaceNode,
  type WorkspacePane,
  type WorkspaceState,
} from "./workspace";
import {
  isPaneViewMode,
  isSessionPaneViewMode,
  isWorkspaceTab,
} from "./workspace-tab-validation";

export const WORKSPACE_LAYOUT_STORAGE_KEY = "termal-workspace-layout";
export const WORKSPACE_VIEW_QUERY_PARAM = "workspace";

export type ControlPanelSide = "left" | "right";

export type StoredWorkspaceLayout = {
  controlPanelSide: ControlPanelSide;
  themeId?: ThemeId;
  styleId?: StyleId;
  markdownThemeId?: MarkdownThemeId;
  markdownStyleId?: MarkdownStyleId;
  fontSizePx?: number;
  editorFontSizePx?: number;
  densityPercent?: number;
  workspace: WorkspaceState;
};

export function createWorkspaceViewId(): string {
  return `workspace-${crypto.randomUUID()}`;
}

export function ensureWorkspaceViewId(): string {
  if (typeof window === "undefined") {
    return "workspace-server";
  }

  const url = new URL(window.location.href);
  const existing = normalizeWorkspaceViewId(url.searchParams.get(WORKSPACE_VIEW_QUERY_PARAM));
  if (existing) {
    return existing;
  }

  const generated = createWorkspaceViewId();
  url.searchParams.set(WORKSPACE_VIEW_QUERY_PARAM, generated);
  window.history.replaceState(window.history.state, "", url.toString());
  return generated;
}

export function getStoredWorkspaceLayout(workspaceViewId: string): StoredWorkspaceLayout | null {
  if (typeof window === "undefined") {
    return null;
  }

  const storageKey = getWorkspaceLayoutStorageKey(workspaceViewId);
  return parseStoredWorkspaceLayout(window.localStorage.getItem(storageKey));
}

export function persistWorkspaceLayout(workspaceViewId: string, layout: StoredWorkspaceLayout) {
  if (typeof window === "undefined") {
    return;
  }

  const persistedLayout = {
    ...layout,
    workspace: stripDiffPreviewDocumentContentFromWorkspaceState(
      stripLoadingGitDiffPreviewTabsFromWorkspaceState(layout.workspace),
    ),
  };

  window.localStorage.setItem(
    getWorkspaceLayoutStorageKey(workspaceViewId),
    JSON.stringify(persistedLayout),
  );
}

export function deleteStoredWorkspaceLayout(workspaceViewId: string) {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.removeItem(getWorkspaceLayoutStorageKey(workspaceViewId));
}

export function parseStoredWorkspaceLayout(raw: string | null | undefined): StoredWorkspaceLayout | null {
  if (!raw) {
    return null;
  }

  try {
    const parsed = JSON.parse(raw);
    if (!isStoredWorkspaceLayout(parsed)) {
      return null;
    }

    return {
      ...parsed,
      workspace: stripDiffPreviewDocumentContentFromWorkspaceState(
        normalizeWorkspaceStatePaths(parsed.workspace),
      ),
    };
  } catch {
    return null;
  }
}

function isStoredWorkspaceLayout(value: unknown): value is StoredWorkspaceLayout {
  if (!isRecord(value)) {
    return false;
  }

  if (!isControlPanelSide(value.controlPanelSide) || !isWorkspaceState(value.workspace)) {
    return false;
  }

  // themeId and styleId are optional - accept absent, reject invalid
  if (value.themeId !== undefined && !isThemeId(value.themeId as string)) {
    return false;
  }
  if (value.styleId !== undefined && !isStyleId(value.styleId as string)) {
    return false;
  }
  if (
    value.markdownThemeId !== undefined &&
    !isMarkdownThemeId(value.markdownThemeId as string)
  ) {
    return false;
  }
  if (
    value.markdownStyleId !== undefined &&
    !isMarkdownStyleId(value.markdownStyleId as string)
  ) {
    return false;
  }

  // Numeric UI settings are optional - accept absent, reject non-finite
  if (value.fontSizePx !== undefined && !isOptionalFiniteNumber(value.fontSizePx)) {
    return false;
  }
  if (value.editorFontSizePx !== undefined && !isOptionalFiniteNumber(value.editorFontSizePx)) {
    return false;
  }
  if (value.densityPercent !== undefined && !isOptionalFiniteNumber(value.densityPercent)) {
    return false;
  }

  return true;
}

function isControlPanelSide(value: unknown): value is ControlPanelSide {
  return value === "left" || value === "right";
}

function getWorkspaceLayoutStorageKey(workspaceViewId: string) {
  const normalizedWorkspaceViewId =
    normalizeWorkspaceViewId(workspaceViewId) ?? "workspace-server";
  return `${WORKSPACE_LAYOUT_STORAGE_KEY}:${normalizedWorkspaceViewId}`;
}

function normalizeWorkspaceViewId(value: string | null | undefined) {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
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

function isOptionalFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
