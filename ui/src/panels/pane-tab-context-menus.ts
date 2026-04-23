// Pure helpers that build the pane-tab right-click context menus
// for git-status and source / diff-preview tabs, plus the typed
// state shape the git context menu uses.
//
// What this file owns:
//   - `GitTabContextMenuAction` — the `"push" | "sync"`
//     enumeration of mutating actions the git-tab menu exposes.
//   - `GitTabContextMenuState` — the full menu state shape
//     (position, pending action, cached `GitStatusResponse`,
//     loading / error flags, workdir, associated session /
//     project ids).
//   - Git-tab label formatters:
//     `formatGitTabContextMenuError`,
//     `formatGitTabBranchMenuLabel`,
//     `formatGitTabUpstreamMenuLabel`,
//     `formatGitTabWorktreeMenuLabel`.
//   - Git-tab action predicates:
//     `canPushGitTabContextMenu`, `canSyncGitTabContextMenu`.
//   - Git-tab menu builder: `buildGitTabContextMenu` — takes a
//     workspace tab + session/project lookups and returns the
//     non-menu-state fields the caller needs to open the menu
//     (`workdir`, `sessionId`, `projectId`), or `null` when the
//     tab isn't a git-status tab or has no resolvable workdir.
//   - Git workdir resolver: `resolveGitTabWorkspaceRoot` — prefers
//     the origin session's workdir, falls back to the project's
//     root path, returns `null` when neither resolves.
//   - File tab menu builder: `buildFileTabContextMenu` — produces
//     `{ path, relativePath }` for a source / diff-preview tab,
//     with the relative path computed against the origin session
//     workdir or project root.
//   - File tab helpers: `getFileTabPath`, `resolveFileTabWorkspaceRoot`,
//     `resolveRelativeTabPath`.
//
// What this file does NOT own:
//   - The context-menu React state or the menu JSX — those stay
//     in `./PaneTabs.tsx` with the `<PaneTabs>` component.
//   - Tab label formatting — `./pane-tab-labels` owns those.
//   - Path display primitives (`looksLikeAbsoluteDisplayPath`,
//     `normalizeDisplayPath`, `relativizePathToWorkspace`) — live
//     in `../path-display`.
//
// Split out of `ui/src/panels/PaneTabs.tsx`. Same types, same
// function bodies, same label copy and allowlist semantics.

import type { GitStatusResponse } from "../api";
import {
  looksLikeAbsoluteDisplayPath,
  normalizeDisplayPath,
  relativizePathToWorkspace,
} from "../path-display";
import type { SessionSummarySnapshot } from "../session-store";
import type { Project } from "../types";
import type { WorkspaceGitStatusTab, WorkspaceTab } from "../workspace";

export type GitTabContextMenuAction = "push" | "sync";

export type GitTabContextMenuState = {
  clientX: number;
  clientY: number;
  isLoadingStatus: boolean;
  pendingAction: GitTabContextMenuAction | null;
  projectId: string | null;
  sessionId: string | null;
  status: GitStatusResponse | null;
  statusError: string | null;
  statusMessage: string | null;
  tabId: string;
  workdir: string;
};

export function formatGitTabContextMenuError(error: unknown) {
  if (error instanceof Error) {
    const message = error.message.trim();
    if (message) {
      return message;
    }
  }

  return "Git action failed.";
}

export function formatGitTabBranchMenuLabel(
  status: GitStatusResponse | null,
  isLoadingStatus: boolean,
) {
  if (isLoadingStatus && !status) {
    return "Branch: Loading...";
  }

  if (!status) {
    return "Branch: Unavailable";
  }

  const branch = status.branch?.trim();
  return `Branch: ${branch || "Detached HEAD"}`;
}

export function formatGitTabUpstreamMenuLabel(
  status: GitStatusResponse | null,
  isLoadingStatus: boolean,
) {
  if (isLoadingStatus && !status) {
    return "Upstream: Loading...";
  }

  if (!status) {
    return "Upstream: Unavailable";
  }

  const upstream = status.upstream?.trim();
  if (!upstream) {
    return "Upstream: Not tracking";
  }

  const position: string[] = [];
  if (status.ahead > 0) {
    position.push(`ahead ${status.ahead}`);
  }
  if (status.behind > 0) {
    position.push(`behind ${status.behind}`);
  }

  const suffix = position.length > 0 ? ` (${position.join(", ")})` : " (up to date)";
  return `Upstream: ${upstream}${suffix}`;
}

export function formatGitTabWorktreeMenuLabel(
  status: GitStatusResponse | null,
  isLoadingStatus: boolean,
) {
  if (isLoadingStatus && !status) {
    return "Status: Loading...";
  }

  if (!status) {
    return "Status: Unavailable";
  }

  if (status.isClean) {
    return "Status: Clean";
  }

  const count = status.files.length;
  return `Status: ${count} changed ${count === 1 ? "file" : "files"}`;
}

export function canPushGitTabContextMenu(menu: GitTabContextMenuState | null) {
  return Boolean(
    menu &&
      !menu.isLoadingStatus &&
      !menu.pendingAction &&
      menu.status?.branch?.trim(),
  );
}

export function canSyncGitTabContextMenu(menu: GitTabContextMenuState | null) {
  return Boolean(
    menu &&
      !menu.isLoadingStatus &&
      !menu.pendingAction &&
      menu.status?.upstream?.trim(),
  );
}

export function buildGitTabContextMenu(
  tab: WorkspaceTab,
  originSession: Pick<SessionSummarySnapshot, "projectId" | "workdir"> | null,
  projectLookup: ReadonlyMap<string, Project>,
) {
  if (tab.kind !== "gitStatus") {
    return null;
  }

  const workdir = tab.workdir?.trim() || resolveGitTabWorkspaceRoot(tab, originSession, projectLookup);
  if (!workdir) {
    return null;
  }

  const projectId = tab.originProjectId ?? originSession?.projectId ?? null;
  return {
    projectId,
    sessionId: tab.originSessionId ?? null,
    workdir,
  };
}

export function resolveGitTabWorkspaceRoot(
  tab: WorkspaceGitStatusTab,
  originSession: Pick<SessionSummarySnapshot, "projectId" | "workdir"> | null,
  projectLookup: ReadonlyMap<string, Project>,
) {
  if (originSession?.workdir) {
    return originSession.workdir;
  }

  const originProjectId = tab.originProjectId ?? originSession?.projectId ?? null;
  return originProjectId ? (projectLookup.get(originProjectId)?.rootPath ?? null) : null;
}

export function buildFileTabContextMenu(
  tab: WorkspaceTab,
  originSession: Pick<SessionSummarySnapshot, "projectId" | "workdir"> | null,
  projectLookup: ReadonlyMap<string, Project>,
) {
  const path = getFileTabPath(tab);
  if (!path) {
    return null;
  }

  const workspaceRoot = resolveFileTabWorkspaceRoot(tab, originSession, projectLookup);
  return {
    path,
    relativePath: resolveRelativeTabPath(path, workspaceRoot),
  };
}

export function getFileTabPath(tab: WorkspaceTab) {
  if (tab.kind === "source") {
    return tab.path?.trim() || null;
  }

  if (tab.kind === "diffPreview") {
    return tab.filePath?.trim() || null;
  }

  return null;
}

export function resolveFileTabWorkspaceRoot(
  tab: WorkspaceTab,
  originSession: Pick<SessionSummarySnapshot, "projectId" | "workdir"> | null,
  projectLookup: ReadonlyMap<string, Project>,
) {
  if (tab.kind !== "source" && tab.kind !== "diffPreview") {
    return null;
  }

  if (originSession?.workdir) {
    return originSession.workdir;
  }

  const originProjectId = tab.originProjectId ?? originSession?.projectId ?? null;
  return originProjectId ? (projectLookup.get(originProjectId)?.rootPath ?? null) : null;
}

export function resolveRelativeTabPath(path: string, workspaceRoot: string | null) {
  const trimmedPath = path.trim();
  if (!trimmedPath) {
    return null;
  }

  if (!looksLikeAbsoluteDisplayPath(trimmedPath)) {
    return normalizeDisplayPath(trimmedPath);
  }

  if (!workspaceRoot) {
    return null;
  }

  const relativePath = relativizePathToWorkspace(trimmedPath, workspaceRoot);
  return relativePath === trimmedPath ? null : relativePath;
}
