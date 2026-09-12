// Owns the shared local-workspace fixture and paramsForLocalWorkspace
// builder used by workspace-layout hook tests. Each call persists a
// fresh layout and returns new mocks/refs.
//
// Does not own: hook behavior, deferred fetch helpers, fake timers,
// or production layout persistence.
//
// Split out of: app-workspace-layout.refresh.test.tsx and
// app-workspace-layout.persistence.test.tsx.
import { vi } from "vitest";

import { createInitialWorkspaceBootstrap } from "./initial-workspace-bootstrap";
import { persistWorkspaceLayout } from "./workspace-storage";
import type { UseAppWorkspaceLayoutParams } from "./app-workspace-layout";
import type { WorkspaceState } from "./workspace-types";

export const localWorkspaceFixture: WorkspaceState = {
  lastContentPaneId: "pane-session", lastViewerPaneId: null,
  activePaneId: "pane-session", root: { type: "pane", paneId: "pane-session" },
  panes: [{ id: "pane-session", activeTabId: "tab-session", activeSessionId: "session-1",
    tabs: [{ id: "tab-session", kind: "session", sessionId: "session-1" }],
    viewMode: "session", lastSessionViewMode: "session", sourcePath: null }],
};

export function paramsForLocalWorkspace(): UseAppWorkspaceLayoutParams {
  const workspaceViewId = "workspace-local-only";
  persistWorkspaceLayout(workspaceViewId, { controlPanelSide: "left", workspace: localWorkspaceFixture });
  const initial = createInitialWorkspaceBootstrap(workspaceViewId);
  return {
    workspaceViewId, workspace: initial.workspace, setWorkspace: vi.fn(),
    sessions: [], sessionsRef: { current: [] }, isSessionStateReady: false,
    controlPanelSide: "left", setControlPanelSide: vi.fn(),
    preferences: initial,
    setPreferences: {
      setThemeId: vi.fn(), setLightThemeId: vi.fn(), setDarkThemeId: vi.fn(),
      setThemeMode: vi.fn(), setStyleId: vi.fn(), setMarkdownThemeId: vi.fn(),
      setMarkdownStyleId: vi.fn(), setDiagramThemeOverrideMode: vi.fn(),
      setDiagramLook: vi.fn(), setDiagramPalette: vi.fn(), setFontSizePx: vi.fn(),
      setEditorFontSizePx: vi.fn(), setDensityPercent: vi.fn(),
    },
    setRequestError: vi.fn(),
    isMountedRef: { current: true }, reportRequestError: vi.fn(),
    applyControlPanelLayout: (value) => value,
  };
}
