import type {
  DiagramLook,
  DiagramPalette,
  DiagramThemeOverrideMode,
  MarkdownStyleId,
  MarkdownThemeId,
  StyleId,
  ThemeId,
  ThemeMode,
} from "./themes";
import {
  isDiagramLook,
  isDiagramPalette,
  isDiagramThemeOverrideMode,
  isMarkdownStyleId,
  isMarkdownThemeId,
  isStyleId,
  isThemeId,
  isThemeMode,
} from "./themes";
import {
  normalizeWorkspaceStatePaths,
  stripDiffPreviewDocumentContentFromWorkspaceState,
  stripLoadingGitDiffPreviewTabsFromWorkspaceState,
} from "./workspace";
import {
  type WorkspaceNode,
  type WorkspacePane,
  type WorkspaceState,
} from "./workspace-types";
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
  lightThemeId?: ThemeId;
  darkThemeId?: ThemeId;
  themeMode?: ThemeMode;
  styleId?: StyleId;
  markdownThemeId?: MarkdownThemeId;
  markdownStyleId?: MarkdownStyleId;
  diagramThemeOverrideMode?: DiagramThemeOverrideMode;
  diagramLook?: DiagramLook;
  diagramPalette?: DiagramPalette;
  fontSizePx?: number;
  editorFontSizePx?: number;
  densityPercent?: number;
  workspace: WorkspaceState;
};

const CURRENT_LAYOUT_FIELDS = [
  "controlPanelSide",
  "lightThemeId",
  "darkThemeId",
  "themeMode",
  "styleId",
  "markdownThemeId",
  "markdownStyleId",
  "diagramThemeOverrideMode",
  "diagramLook",
  "diagramPalette",
  "fontSizePx",
  "editorFontSizePx",
  "densityPercent",
  "workspace",
] as const satisfies readonly (keyof StoredWorkspaceLayout)[];

function currentLayoutFields(layout: StoredWorkspaceLayout): StoredWorkspaceLayout {
  // Unknown fields never become persistence authority or get mirrored on save.
  return Object.fromEntries(
    CURRENT_LAYOUT_FIELDS.filter((key) =>
      Object.prototype.hasOwnProperty.call(layout, key),
    ).map((key) => [key, layout[key]]),
  ) as StoredWorkspaceLayout;
}

export function createWorkspaceViewId(): string {
  return `workspace-${crypto.randomUUID()}`;
}

export function ensureWorkspaceViewId(): string {
  if (typeof window === "undefined") {
    return "workspace-server";
  }

  const url = new URL(window.location.href);
  const existing = normalizeWorkspaceViewId(
    url.searchParams.get(WORKSPACE_VIEW_QUERY_PARAM),
  );
  if (existing) {
    return existing;
  }

  const generated = createWorkspaceViewId();
  url.searchParams.set(WORKSPACE_VIEW_QUERY_PARAM, generated);
  window.history.replaceState(window.history.state, "", url.toString());
  return generated;
}

export function getStoredWorkspaceLayout(
  workspaceViewId: string,
): StoredWorkspaceLayout | null {
  if (typeof window === "undefined") {
    return null;
  }

  const storageKey = getWorkspaceLayoutStorageKey(workspaceViewId);
  return parseStoredWorkspaceLayout(window.localStorage.getItem(storageKey));
}

export function persistWorkspaceLayout(
  workspaceViewId: string,
  layout: StoredWorkspaceLayout,
) {
  if (typeof window === "undefined") {
    return;
  }

  const persistedLayout = {
    ...currentLayoutFields(layout),
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

export function parseStoredWorkspaceLayout(
  raw: string | null | undefined,
): StoredWorkspaceLayout | null {
  if (!raw) {
    return null;
  }

  try {
    const parsed = normalizeStoredWorkspaceRoutingHints(
      normalizeStoredWorkspaceTabVisitHistories(
        normalizeStoredDiagramLook(JSON.parse(raw)),
      ),
    );
    if (!isStoredWorkspaceLayout(parsed)) {
      return null;
    }

    return {
      ...currentLayoutFields(parsed),
      workspace: stripDiffPreviewDocumentContentFromWorkspaceState(
        normalizeWorkspaceStatePaths(parsed.workspace),
      ),
    };
  } catch {
    return null;
  }
}

function normalizeStoredWorkspaceRoutingHints(value: unknown): unknown {
  if (
    !isRecord(value) ||
    !isRecord(value.workspace) ||
    !Array.isArray(value.workspace.panes)
  ) {
    return value;
  }

  const paneIds = new Set(
    value.workspace.panes.flatMap((pane) =>
      isRecord(pane) && isString(pane.id) ? [pane.id] : [],
    ),
  );
  const workspace = value.workspace;
  let normalizedWorkspace: Record<string, unknown> | null = null;
  for (const key of ["lastContentPaneId", "lastViewerPaneId"] as const) {
    const paneId = workspace[key];
    if (
      paneId === undefined ||
      paneId === null ||
      (typeof paneId === "string" && paneIds.has(paneId))
    ) {
      continue;
    }

    normalizedWorkspace ??= { ...workspace };
    normalizedWorkspace[key] = null;
  }

  return normalizedWorkspace
    ? { ...value, workspace: normalizedWorkspace }
    : value;
}

function normalizeStoredWorkspaceTabVisitHistories(value: unknown): unknown {
  if (
    !isRecord(value) ||
    !isRecord(value.workspace) ||
    !Array.isArray(value.workspace.panes)
  ) {
    return value;
  }

  let changed = false;
  const panes = value.workspace.panes.map((pane) => {
    if (
      !isRecord(pane) ||
      typeof pane.tabVisitHistory === "undefined" ||
      hasValidPaneTabVisitHistory(pane)
    ) {
      return pane;
    }

    const normalizedPane = { ...pane };
    delete normalizedPane.tabVisitHistory;
    changed = true;
    return normalizedPane;
  });

  if (!changed) {
    return value;
  }

  return {
    ...value,
    workspace: {
      ...value.workspace,
      panes,
    },
  };
}

function normalizeStoredDiagramLook(value: unknown): unknown {
  if (
    !isRecord(value) ||
    value.diagramLook === undefined ||
    isDiagramLook(value.diagramLook as string)
  ) {
    return value;
  }
  // An unknown cosmetic preference has no authority over the pane arrangement.
  // Drop it generically so the current preference/default can supply the look.
  const { diagramLook: _ignored, ...layout } = value;
  return layout;
}

function isStoredWorkspaceLayout(
  value: unknown,
): value is StoredWorkspaceLayout {
  if (!isRecord(value)) {
    return false;
  }

  if (
    !isControlPanelSide(value.controlPanelSide) ||
    !isWorkspaceState(value.workspace)
  ) {
    return false;
  }

  // Current appearance preferences may be absent to use their defaults.
  if (
    value.lightThemeId !== undefined &&
    !isThemeId(value.lightThemeId as string)
  ) {
    return false;
  }
  if (
    value.darkThemeId !== undefined &&
    !isThemeId(value.darkThemeId as string)
  ) {
    return false;
  }
  if (
    value.themeMode !== undefined &&
    !isThemeMode(value.themeMode as string)
  ) {
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
  if (
    value.diagramThemeOverrideMode !== undefined &&
    !isDiagramThemeOverrideMode(value.diagramThemeOverrideMode as string)
  ) {
    return false;
  }
  if (
    value.diagramLook !== undefined &&
    !isDiagramLook(value.diagramLook as string)
  ) {
    return false;
  }
  if (
    value.diagramPalette !== undefined &&
    !isDiagramPalette(value.diagramPalette as string)
  ) {
    return false;
  }

  // Numeric UI settings are optional - accept absent, reject non-finite
  if (
    value.fontSizePx !== undefined &&
    !isOptionalFiniteNumber(value.fontSizePx)
  ) {
    return false;
  }
  if (
    value.editorFontSizePx !== undefined &&
    !isOptionalFiniteNumber(value.editorFontSizePx)
  ) {
    return false;
  }
  if (
    value.densityPercent !== undefined &&
    !isOptionalFiniteNumber(value.densityPercent)
  ) {
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
  if (
    !isRecord(value) ||
    !Array.isArray(value.panes) ||
    !isNullableString(value.activePaneId) ||
    !isNullableString(value.lastContentPaneId) ||
    !isNullableString(value.lastViewerPaneId)
  ) {
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
  if (
    value.lastContentPaneId !== null &&
    !paneIds.has(value.lastContentPaneId)
  ) {
    return false;
  }
  if (
    value.lastViewerPaneId !== null &&
    !paneIds.has(value.lastViewerPaneId)
  ) {
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
    hasValidPaneTabVisitHistory(value) &&
    isNullableString(value.activeSessionId) &&
    isPaneViewMode(value.viewMode) &&
    isSessionPaneViewMode(value.lastSessionViewMode) &&
    isNullableString(value.sourcePath)
  );
}

// Keep this persisted boundary contract aligned with the history producers in
// `workspace-tab-history.ts`.
function hasValidPaneTabVisitHistory(pane: Record<string, unknown>): boolean {
  const tabVisitHistory = pane.tabVisitHistory;
  if (typeof tabVisitHistory === "undefined") {
    return true;
  }
  if (!Array.isArray(pane.tabs)) {
    return false;
  }

  const tabIds = new Set(
    pane.tabs.flatMap((tab) =>
      isRecord(tab) && isString(tab.id) ? [tab.id] : [],
    ),
  );
  return (
    Array.isArray(tabVisitHistory) &&
    tabVisitHistory.every((tabId) => isString(tabId) && tabIds.has(tabId)) &&
    new Set(tabVisitHistory).size === tabVisitHistory.length &&
    (pane.activeTabId === null
      ? tabVisitHistory.length === 0
      : tabVisitHistory[0] === pane.activeTabId)
  );
}

function isWorkspaceNode(
  value: unknown,
  paneIds: ReadonlySet<string>,
): value is WorkspaceNode {
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
  return (
    typeof value === "number" &&
    Number.isFinite(value) &&
    value > 0 &&
    value < 1
  );
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
