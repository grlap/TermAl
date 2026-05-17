// Owns: active workspace-tab selection for SessionPaneView.
// Does not own: tab rendering, workspace tree mutation, or view-mode fallback behavior.
// Split from: ui/src/SessionPaneView.tsx.

import type { WorkspacePane, WorkspaceTab } from "./workspace";

export function resolveSessionPaneActiveTab(
  pane: Pick<WorkspacePane, "activeTabId" | "tabs">,
): WorkspaceTab | null {
  return (
    pane.tabs.find((tab) => tab.id === pane.activeTabId) ??
    pane.tabs[0] ??
    null
  );
}
