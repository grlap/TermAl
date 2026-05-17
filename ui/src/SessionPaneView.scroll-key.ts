// Owns: the stable scroll-position key resolver for SessionPaneView panes.
// Does not own: scroll restoration effects, DOM measurement, or pane rendering.
// Split from: ui/src/SessionPaneView.tsx.

import type { PaneViewMode, WorkspaceTab } from "./workspace";

export function resolveSessionPaneScrollStateKey(
  paneId: string,
  viewMode: PaneViewMode,
  activeSessionId: string | null | undefined,
  activeTab: WorkspaceTab | null | undefined,
) {
  switch (activeTab?.kind) {
    case "source":
      return `${paneId}:source:${activeTab.path ?? "empty"}`;
    case "canvas":
      return `${paneId}:canvas:${activeTab.id}`;
    case "orchestratorCanvas":
      return `${paneId}:orchestratorCanvas:${activeTab.id}`;
    case "filesystem":
      return `${paneId}:filesystem:${activeTab.rootPath ?? "empty"}`;
    case "gitStatus":
      return `${paneId}:gitStatus:${activeTab.workdir ?? "empty"}`;
    case "terminal":
      return `${paneId}:terminal:${activeTab.id}`;
    case "instructionDebugger":
      return `${paneId}:instructionDebugger:${
        activeTab.originSessionId ?? activeTab.workdir ?? "empty"
      }`;
    case "diffPreview":
      return `${paneId}:diffPreview:${activeTab.diffMessageId}`;
    default:
      return `${paneId}:${viewMode}:${activeSessionId ?? "empty"}`;
  }
}
