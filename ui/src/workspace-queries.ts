// Pure query helpers over the workspace tree.
//
// What this file owns:
//   - `workspaceNodeContainsControlPanel` — recursive "does this
//     subtree contain a pane whose tabs include the control panel?"
//     query, used by layout code to pick minimum pane widths.
//   - `getActiveWorkspacePaneTab` — reads the tab that is currently
//     active in a pane (the one whose id matches `activeTabId`, or
//     the first tab as a fallback).
//   - `paneHasActiveStandaloneControlSurface` — "is the currently
//     active tab a control-surface kind other than the control
//     panel?" (filesystem list, git status, etc.).
//   - `workspaceNodeContainsStandaloneControlSurface` — recursive
//     version of the above across a whole subtree.
//   - `workspaceNodeContainsNonControlSurfacePane` — recursive "does
//     this subtree contain a pane whose active tab is NOT a control
//     surface?" query.
//   - `workspaceNodeUsesStandaloneControlSurfaceMinWidth` — shortcut
//     for "should this subtree be sized with the narrower
//     standalone-control-surface minimum width?" (true when it has
//     standalone control surfaces AND no control panel AND no
//     non-control-surface panes).
//   - `findWorkspaceSplitNode` — walks the tree to find a split node
//     by id, or `null` if absent.
//   - `workspaceContainsOnlyControlPanel` — "is this workspace a
//     single pane with only the control panel in it?" — used to
//     decide whether the stage should render in
//     control-panel-only mode.
//   - `getWorkspaceSplitResizeBounds` — given a split id and its
//     rendered size in pixels, returns the `{ minRatio, maxRatio }`
//     the split handle may resize between. Accounts for the
//     control-panel and standalone-control-surface minimum widths
//     by reading their CSS variables via
//     `resolveRootCssLengthPx`.
//   - `resolveControlSurfaceSectionIdForWorkspaceTab` — maps a
//     workspace tab to the `ControlPanelSectionId` it represents
//     (`files`/`git`/`projects`/`sessions`/`orchestrators`) or
//     `null` when the tab isn't a control-surface tab. Exhaustive
//     switch over `tab.kind`; new tab kinds have to opt in
//     explicitly.
//   - `resolveWorkspaceTabProjectId` — given a workspace tab and a
//     session lookup, returns the project id the tab is scoped to.
//     Handles session tabs (direct `sessionId` lookup) and
//     surface tabs (falls back to `originProjectId` on the tab,
//     then to the origin session's project).
//
// What this file does NOT own:
//   - Workspace tree mutations (split, close, move tab, reconcile) —
//     those live in `./workspace.ts`.
//   - React state or components — the functions are pure.
//   - The CSS variables themselves, the viewport-reading heuristics,
//     and the control-panel-specific layout constants — those live
//     in `./control-panel-layout.ts`. This module depends on them,
//     not the other way round.
//
// Split out of `ui/src/App.tsx`. Same function signatures and
// behaviour as the inline definitions they replaced. Kept separate
// from `workspace.ts` so `workspace.ts` stays focused on tree
// operations; kept separate from `control-panel-layout.ts` so that
// module stays focused on pane-width math rather than generic tree
// queries.

import { clamp } from "./app-utils";
import {
  CONTROL_PANEL_PANE_MIN_WIDTH_FALLBACK_PX,
  DEFAULT_SPLIT_MAX_RATIO,
  DEFAULT_SPLIT_MIN_RATIO,
  STANDALONE_CONTROL_SURFACE_PANE_MIN_WIDTH_FALLBACK_PX,
  resolveRootCssLengthPx,
} from "./control-panel-layout";
import type { ControlPanelSectionId } from "./panels/ControlPanelSurface";
import type { Session } from "./types";
import {
  CONTROL_SURFACE_KINDS,
  type WorkspaceNode,
  type WorkspacePane,
  type WorkspaceState,
  type WorkspaceTab,
} from "./workspace";

export function workspaceNodeContainsControlPanel(
  node: WorkspaceNode,
  paneLookup: Map<string, WorkspacePane>,
): boolean {
  if (node.type === "pane") {
    return (
      paneLookup
        .get(node.paneId)
        ?.tabs.some((tab) => tab.kind === "controlPanel") ?? false
    );
  }

  return (
    workspaceNodeContainsControlPanel(node.first, paneLookup) ||
    workspaceNodeContainsControlPanel(node.second, paneLookup)
  );
}

export function getActiveWorkspacePaneTab(pane: WorkspacePane): WorkspaceTab | null {
  return (
    pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? pane.tabs[0] ?? null
  );
}

export function paneHasActiveStandaloneControlSurface(pane: WorkspacePane): boolean {
  const activeTab = getActiveWorkspacePaneTab(pane);
  return Boolean(
    activeTab &&
    activeTab.kind !== "controlPanel" &&
    CONTROL_SURFACE_KINDS.has(activeTab.kind),
  );
}

export function workspaceNodeContainsStandaloneControlSurface(
  node: WorkspaceNode,
  paneLookup: Map<string, WorkspacePane>,
): boolean {
  if (node.type === "pane") {
    const pane = paneLookup.get(node.paneId);
    return pane ? paneHasActiveStandaloneControlSurface(pane) : false;
  }

  return (
    workspaceNodeContainsStandaloneControlSurface(node.first, paneLookup) ||
    workspaceNodeContainsStandaloneControlSurface(node.second, paneLookup)
  );
}

export function workspaceNodeContainsNonControlSurfacePane(
  node: WorkspaceNode,
  paneLookup: Map<string, WorkspacePane>,
): boolean {
  if (node.type === "pane") {
    const pane = paneLookup.get(node.paneId);
    const activeTab = pane ? getActiveWorkspacePaneTab(pane) : null;
    return activeTab ? !CONTROL_SURFACE_KINDS.has(activeTab.kind) : false;
  }

  return (
    workspaceNodeContainsNonControlSurfacePane(node.first, paneLookup) ||
    workspaceNodeContainsNonControlSurfacePane(node.second, paneLookup)
  );
}

export function workspaceNodeUsesStandaloneControlSurfaceMinWidth(
  node: WorkspaceNode,
  paneLookup: Map<string, WorkspacePane>,
): boolean {
  return (
    !workspaceNodeContainsControlPanel(node, paneLookup) &&
    !workspaceNodeContainsNonControlSurfacePane(node, paneLookup) &&
    workspaceNodeContainsStandaloneControlSurface(node, paneLookup)
  );
}

export function findWorkspaceSplitNode(
  node: WorkspaceNode | null,
  splitId: string,
): Extract<WorkspaceNode, { type: "split" }> | null {
  if (!node || node.type === "pane") {
    return null;
  }

  if (node.id === splitId) {
    return node;
  }

  return (
    findWorkspaceSplitNode(node.first, splitId) ??
    findWorkspaceSplitNode(node.second, splitId)
  );
}

export function workspaceContainsOnlyControlPanel(workspace: WorkspaceState) {
  return (
    workspace.panes.length === 1 &&
    workspace.panes[0]?.tabs.length === 1 &&
    workspace.panes[0]?.tabs[0]?.kind === "controlPanel"
  );
}

export function getWorkspaceSplitResizeBounds(
  root: WorkspaceNode | null,
  splitId: string,
  direction: "row" | "column",
  size: number,
  paneLookup: Map<string, WorkspacePane>,
): { minRatio: number; maxRatio: number } {
  if (direction !== "row" || size <= 0) {
    return {
      minRatio: DEFAULT_SPLIT_MIN_RATIO,
      maxRatio: DEFAULT_SPLIT_MAX_RATIO,
    };
  }

  const splitNode = findWorkspaceSplitNode(root, splitId);
  if (!splitNode) {
    return {
      minRatio: DEFAULT_SPLIT_MIN_RATIO,
      maxRatio: DEFAULT_SPLIT_MAX_RATIO,
    };
  }

  const controlPanelMinRatio = clamp(
    resolveRootCssLengthPx(
      "--control-panel-pane-min-width",
      CONTROL_PANEL_PANE_MIN_WIDTH_FALLBACK_PX,
    ) / size,
    0,
    1,
  );
  const standaloneControlSurfaceMinRatio = clamp(
    resolveRootCssLengthPx(
      "--standalone-control-surface-pane-min-width",
      STANDALONE_CONTROL_SURFACE_PANE_MIN_WIDTH_FALLBACK_PX,
    ) / size,
    0,
    1,
  );
  const firstMinRatio = workspaceNodeContainsControlPanel(
    splitNode.first,
    paneLookup,
  )
    ? controlPanelMinRatio
    : workspaceNodeUsesStandaloneControlSurfaceMinWidth(
          splitNode.first,
          paneLookup,
        )
      ? standaloneControlSurfaceMinRatio
      : DEFAULT_SPLIT_MIN_RATIO;
  const secondMinRatio = workspaceNodeContainsControlPanel(
    splitNode.second,
    paneLookup,
  )
    ? controlPanelMinRatio
    : workspaceNodeUsesStandaloneControlSurfaceMinWidth(
          splitNode.second,
          paneLookup,
        )
      ? standaloneControlSurfaceMinRatio
      : DEFAULT_SPLIT_MIN_RATIO;
  const minRatio = firstMinRatio;
  const maxRatio = 1 - secondMinRatio;

  if (minRatio <= maxRatio) {
    return {
      minRatio,
      maxRatio,
    };
  }

  const constrainedRatio = clamp(
    firstMinRatio / Math.max(firstMinRatio + secondMinRatio, Number.EPSILON),
    0,
    1,
  );

  return {
    minRatio: constrainedRatio,
    maxRatio: constrainedRatio,
  };
}

export function resolveControlSurfaceSectionIdForWorkspaceTab(
  tab: WorkspaceTab,
): ControlPanelSectionId | null {
  switch (tab.kind) {
    case "filesystem":
      return "files";
    case "gitStatus":
      return "git";
    case "orchestratorList":
      return "orchestrators";
    case "projectList":
      return "projects";
    case "sessionList":
      return "sessions";
    case "session":
    case "source":
    case "controlPanel":
    case "canvas":
    case "orchestratorCanvas":
    case "terminal":
    case "instructionDebugger":
    case "diffPreview":
      return null;
  }
}

export function resolveWorkspaceTabProjectId(
  tab: WorkspaceTab | undefined,
  sessionLookup: Map<string, Session>,
): string | null {
  if (!tab) {
    return null;
  }

  if (tab.kind === "session") {
    return sessionLookup.get(tab.sessionId)?.projectId ?? null;
  }

  const originSession =
    "originSessionId" in tab && tab.originSessionId
      ? (sessionLookup.get(tab.originSessionId) ?? null)
      : null;
  return (
    ("originProjectId" in tab ? tab.originProjectId : null) ??
    originSession?.projectId ??
    null
  );
}
