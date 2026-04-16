import type { GitDiffDocumentContent, GitDiffRequestPayload, GitDiffSection } from "./api";
import type { DiffMessage, Session } from "./types";

export type SessionPaneViewMode = "session" | "prompt" | "commands" | "diffs";
export type PaneViewMode =
  | SessionPaneViewMode
  | "canvas"
  | "controlPanel"
  | "orchestratorList"
  | "orchestratorCanvas"
  | "sessionList"
  | "projectList"
  | "source"
  | "filesystem"
  | "gitStatus"
  | "terminal"
  | "instructionDebugger"
  | "diffPreview";

export type WorkspaceSessionTab = {
  id: string;
  kind: "session";
  sessionId: string;
};

export type WorkspaceSourceTab = {
  id: string;
  kind: "source";
  path: string | null;
  focusLineNumber?: number | null;
  focusColumnNumber?: number | null;
  focusToken?: string | null;
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceFilesystemTab = {
  id: string;
  kind: "filesystem";
  rootPath: string | null;
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceGitStatusTab = {
  id: string;
  kind: "gitStatus";
  workdir: string | null;
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceTerminalTab = {
  id: string;
  kind: "terminal";
  workdir: string | null;
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceControlPanelTab = {
  id: string;
  kind: "controlPanel";
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceOrchestratorListTab = {
  id: string;
  kind: "orchestratorList";
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceCanvasCard = {
  sessionId: string;
  x: number;
  y: number;
};

export type WorkspaceCanvasTab = {
  id: string;
  kind: "canvas";
  cards: WorkspaceCanvasCard[];
  zoom?: number;
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceOrchestratorCanvasTab = {
  id: string;
  kind: "orchestratorCanvas";
  originSessionId: string | null;
  originProjectId?: string | null;
  templateId?: string | null;
  startMode?: "new";
};

export type WorkspaceSessionListTab = {
  id: string;
  kind: "sessionList";
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceProjectListTab = {
  id: string;
  kind: "projectList";
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceInstructionDebuggerTab = {
  id: string;
  kind: "instructionDebugger";
  workdir: string | null;
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceDiffPreviewTab = {
  id: string;
  kind: "diffPreview";
  changeType: DiffMessage["changeType"];
  changeSetId?: string | null;
  diff: string;
  documentEnrichmentNote?: string | null;
  documentContent?: GitDiffDocumentContent | null;
  diffMessageId: string;
  filePath: string | null;
  gitSectionId?: GitDiffSection | null;
  language?: string | null;
  originSessionId: string | null;
  originProjectId?: string | null;
  summary: string;
  gitDiffRequestKey?: string | null;
  gitDiffRequest?: GitDiffRequestPayload | null;
  isLoading?: boolean;
  loadError?: string | null;
};

export type WorkspaceTab =
  | WorkspaceSessionTab
  | WorkspaceSourceTab
  | WorkspaceFilesystemTab
  | WorkspaceGitStatusTab
  | WorkspaceTerminalTab
  | WorkspaceControlPanelTab
  | WorkspaceOrchestratorListTab
  | WorkspaceCanvasTab
  | WorkspaceOrchestratorCanvasTab
  | WorkspaceSessionListTab
  | WorkspaceProjectListTab
  | WorkspaceInstructionDebuggerTab
  | WorkspaceDiffPreviewTab;

type WorkspaceOriginOnlyTab =
  | WorkspaceControlPanelTab
  | WorkspaceOrchestratorListTab
  | WorkspaceSessionListTab
  | WorkspaceProjectListTab;

export type WorkspacePane = {
  id: string;
  tabs: WorkspaceTab[];
  activeTabId: string | null;
  activeSessionId: string | null;
  viewMode: PaneViewMode;
  lastSessionViewMode: SessionPaneViewMode;
  sourcePath: string | null;
};

type WorkspaceSourceFocus = {
  line: number | null;
  column: number | null;
  token: string | null;
};

type OpenSourceTabOptions = {
  line?: number | null;
  column?: number | null;
  openInNewTab?: boolean;
};

export type WorkspaceNode =
  | {
      type: "pane";
      paneId: string;
    }
  | {
      id: string;
      type: "split";
      direction: "row" | "column";
      ratio: number;
      first: WorkspaceNode;
      second: WorkspaceNode;
    };

export type WorkspaceState = {
  root: WorkspaceNode | null;
  panes: WorkspacePane[];
  activePaneId: string | null;
};

export type TabDropPlacement = "left" | "right" | "top" | "bottom" | "tabs";
export const DEFAULT_CONTROL_PANEL_DOCK_WIDTH_RATIO = 0.24;
export const WORKSPACE_CANVAS_DEFAULT_ZOOM = 1;
export const WORKSPACE_CANVAS_MIN_ZOOM = 0.5;
export const WORKSPACE_CANVAS_MAX_ZOOM = 2;
const DEFAULT_ADJACENT_PANE_SPLIT_RATIO = 0.5;

export function createSessionTab(sessionId: string): WorkspaceSessionTab {
  return {
    id: crypto.randomUUID(),
    kind: "session",
    sessionId,
  };
}

export function createSourceTab(
  path: string | null = null,
  originSessionId: string | null = null,
  originProjectId: string | null = null,
  focus: WorkspaceSourceFocus = EMPTY_WORKSPACE_SOURCE_FOCUS,
): WorkspaceSourceTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);
  const normalizedFocus = normalizeWorkspaceSourceFocus(focus);

  return {
    id: crypto.randomUUID(),
    kind: "source",
    path: normalizeWorkspacePath(path),
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
    ...sourceFocusProps(normalizedFocus),
  };
}

export function createFilesystemTab(
  rootPath: string | null = null,
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceFilesystemTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "filesystem",
    rootPath: normalizeWorkspacePath(rootPath),
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createGitStatusTab(
  workdir: string | null = null,
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceGitStatusTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "gitStatus",
    workdir: normalizeWorkspacePath(workdir),
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createTerminalTab(
  workdir: string | null = null,
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceTerminalTab {
  const normalizedOriginSessionId = normalizeWorkspaceIdentifier(originSessionId);
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "terminal",
    workdir: normalizeWorkspacePath(workdir),
    originSessionId: normalizedOriginSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createControlPanelTab(
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceControlPanelTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "controlPanel",
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createOrchestratorListTab(
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceOrchestratorListTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "orchestratorList",
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createCanvasTab(
  originSessionId: string | null = null,
  originProjectId: string | null = null,
  cards: readonly WorkspaceCanvasCard[] = [],
  zoom: number = WORKSPACE_CANVAS_DEFAULT_ZOOM,
): WorkspaceCanvasTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "canvas",
    cards: normalizeWorkspaceCanvasCards(cards),
    ...canvasZoomProps(normalizeWorkspaceCanvasZoom(zoom)),
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createOrchestratorCanvasTab(
  originSessionId: string | null = null,
  originProjectId: string | null = null,
  templateId: string | null = null,
  startMode: "new" | null = null,
): WorkspaceOrchestratorCanvasTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);
  const normalizedTemplateId = normalizeWorkspaceIdentifier(templateId);

  return {
    id: crypto.randomUUID(),
    kind: "orchestratorCanvas",
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
    ...(normalizedTemplateId ? { templateId: normalizedTemplateId } : {}),
    ...(startMode === "new" ? { startMode } : {}),
  };
}

export function createSessionListTab(
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceSessionListTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "sessionList",
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createProjectListTab(
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceProjectListTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "projectList",
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createInstructionDebuggerTab(
  workdir: string | null = null,
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceInstructionDebuggerTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "instructionDebugger",
    workdir: normalizeWorkspacePath(workdir),
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createDiffPreviewTab({
  changeType,
  changeSetId = null,
  diff,
  documentEnrichmentNote = null,
  documentContent = null,
  diffMessageId,
  filePath = null,
  gitSectionId = null,
  language = null,
  originSessionId = null,
  originProjectId = null,
  summary,
  gitDiffRequestKey = null,
  gitDiffRequest = null,
  isLoading = false,
  loadError = null,
}: {
  changeType: DiffMessage["changeType"];
  changeSetId?: string | null;
  diff: string;
  documentEnrichmentNote?: string | null;
  documentContent?: GitDiffDocumentContent | null;
  diffMessageId: string;
  filePath?: string | null;
  gitSectionId?: GitDiffSection | null;
  language?: string | null;
  originSessionId?: string | null;
  originProjectId?: string | null;
  summary: string;
  gitDiffRequestKey?: string | null;
  gitDiffRequest?: GitDiffRequestPayload | null;
  isLoading?: boolean;
  loadError?: string | null;
}): WorkspaceDiffPreviewTab {
  const normalizedChangeSetId = normalizeWorkspaceIdentifier(changeSetId);
  const normalizedDocumentEnrichmentNote = normalizeWorkspaceText(documentEnrichmentNote);
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);
  const normalizedGitDiffRequestKey = normalizeWorkspaceIdentifier(gitDiffRequestKey);
  const normalizedLoadError = normalizeWorkspaceIdentifier(loadError);

  return {
    id: crypto.randomUUID(),
    kind: "diffPreview",
    changeType,
    ...(normalizedChangeSetId ? { changeSetId: normalizedChangeSetId } : {}),
    diff,
    ...(normalizedDocumentEnrichmentNote ? { documentEnrichmentNote: normalizedDocumentEnrichmentNote } : {}),
    ...(documentContent ? { documentContent } : {}),
    diffMessageId,
    filePath: normalizeWorkspacePath(filePath),
    ...(gitSectionId ? { gitSectionId } : {}),
    language,
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
    summary,
    ...(normalizedGitDiffRequestKey ? { gitDiffRequestKey: normalizedGitDiffRequestKey } : {}),
    ...(gitDiffRequest ? { gitDiffRequest } : {}),
    ...(isLoading ? { isLoading: true } : {}),
    ...(normalizedLoadError ? { loadError: normalizedLoadError } : {}),
  };
}

export function normalizeWorkspaceStatePaths(workspace: WorkspaceState): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) =>
      syncPaneState({
        ...pane,
        sourcePath: normalizeWorkspacePath(pane.sourcePath),
        tabs: pane.tabs.map((tab) => {
          if (tab.kind === "source") {
            return {
              ...tab,
              path: normalizeWorkspacePath(tab.path),
            };
          }
          // IMPORTANT: every branch below uses `...tab` spread to preserve
          // fields other than the normalized path. That is load-bearing for
          // `originSessionId` on all branches and for `originProjectId` on
          // the terminal branch in particular — both drive remote-scope
          // resolution in `TerminalPanel`. The spread pattern is asymmetric
          // with `reconcileWorkspaceState` below, which explicitly
          // destructures and re-attaches origin fields via
          // `projectOriginProps(...)`. If you rewrite this normalizer to
          // enumerate fields explicitly (e.g., to match the reducer's
          // style), use `projectOriginProps(...)` to re-attach origins or
          // you will silently drop them for every tab that hasn't been
          // reconciled yet. The round-trip tests in
          // `ui/src/workspace-storage.test.ts` pin `originProjectId` on
          // terminal tabs; keep those green.
          if (tab.kind === "filesystem") {
            return {
              ...tab,
              rootPath: normalizeWorkspacePath(tab.rootPath),
            };
          }
          if (
            tab.kind === "gitStatus" ||
            tab.kind === "terminal" ||
            tab.kind === "instructionDebugger"
          ) {
            return {
              ...tab,
              workdir: normalizeWorkspacePath(tab.workdir),
            };
          }
          if (tab.kind === "diffPreview") {
            return {
              ...tab,
              filePath: normalizeWorkspacePath(tab.filePath),
            };
          }
          return tab;
        }),
      }),
    ),
  };
}

export function reconcileWorkspaceState(current: WorkspaceState, sessions: Session[]): WorkspaceState {
  const availableSessionIds = new Set(sessions.map((session) => session.id));
  let panes = current.panes.map((pane) => {
    const tabs = pane.tabs
      .flatMap((tab): WorkspaceTab[] => {
        if (tab.kind === "session") {
          return availableSessionIds.has(tab.sessionId) ? [tab] : [];
        }

        const originSessionId =
          tab.originSessionId && availableSessionIds.has(tab.originSessionId)
            ? tab.originSessionId
            : null;
        const originProjectId = normalizeWorkspaceIdentifier(tab.originProjectId);

        if (tab.kind === "source") {
          const {
            originProjectId: _ignoredOriginProjectId,
            focusLineNumber: _ignoredFocusLineNumber,
            focusColumnNumber: _ignoredFocusColumnNumber,
            focusToken: _ignoredFocusToken,
            ...tabWithoutOriginProjectId
          } = tab;
          return [
            {
              ...tabWithoutOriginProjectId,
              originSessionId,
              ...projectOriginProps(originProjectId),
              path: normalizeWorkspacePath(tab.path),
              ...sourceFocusProps(
                normalizeWorkspaceSourceFocus({
                  line: tab.focusLineNumber ?? null,
                  column: tab.focusColumnNumber ?? null,
                  token: tab.focusToken ?? null,
                }),
              ),
            },
          ];
        }

        if (tab.kind === "filesystem") {
          const { originProjectId: _ignoredOriginProjectId, ...tabWithoutOriginProjectId } = tab;
          return [
            {
              ...tabWithoutOriginProjectId,
              originSessionId,
              ...projectOriginProps(originProjectId),
              rootPath: normalizeWorkspacePath(tab.rootPath),
            },
          ];
        }

        if (tab.kind === "gitStatus") {
          const { originProjectId: _ignoredOriginProjectId, ...tabWithoutOriginProjectId } = tab;
          return [
            {
              ...tabWithoutOriginProjectId,
              originSessionId,
              ...projectOriginProps(originProjectId),
              workdir: normalizeWorkspacePath(tab.workdir),
            },
          ];
        }

        if (tab.kind === "terminal") {
          const { originProjectId: _ignoredOriginProjectId, ...tabWithoutOriginProjectId } = tab;
          return [
            {
              ...tabWithoutOriginProjectId,
              originSessionId,
              ...projectOriginProps(originProjectId),
              workdir: normalizeWorkspacePath(tab.workdir),
            },
          ];
        }

        if (
          tab.kind === "controlPanel" ||
          tab.kind === "orchestratorList" ||
          tab.kind === "sessionList" ||
          tab.kind === "projectList"
        ) {
          return [reconcileOriginOnlyTab(tab, originSessionId, originProjectId)];
        }

        if (tab.kind === "canvas") {
          const { originProjectId: _ignoredOriginProjectId, zoom: _ignoredZoom, ...tabWithoutOriginProjectId } = tab;
          return [
            {
              ...tabWithoutOriginProjectId,
              cards: normalizeWorkspaceCanvasCards(tab.cards).filter((card) =>
                availableSessionIds.has(card.sessionId)
              ),
              ...canvasZoomProps(normalizeWorkspaceCanvasZoom(tab.zoom)),
              originSessionId,
              ...projectOriginProps(originProjectId),
            },
          ];
        }

        if (tab.kind === "orchestratorCanvas") {
          const {
            originProjectId: _ignoredOriginProjectId,
            templateId: _ignoredTemplateId,
            startMode: _ignoredStartMode,
            ...tabWithoutSpecialFields
          } = tab;
          const normalizedTemplateId = normalizeWorkspaceIdentifier(tab.templateId);
          return [
            {
              ...tabWithoutSpecialFields,
              originSessionId,
              ...projectOriginProps(originProjectId),
              ...(normalizedTemplateId ? { templateId: normalizedTemplateId } : {}),
              ...(tab.startMode === "new" ? { startMode: "new" as const } : {}),
            },
          ];
        }

        if (tab.kind === "instructionDebugger") {
          const { originProjectId: _ignoredOriginProjectId, ...tabWithoutOriginProjectId } = tab;
          return [
            {
              ...tabWithoutOriginProjectId,
              originSessionId,
              ...projectOriginProps(originProjectId),
              workdir: normalizeWorkspacePath(tab.workdir),
            },
          ];
        }

        const { originProjectId: _ignoredOriginProjectId, ...tabWithoutOriginProjectId } = tab;
        const normalizedChangeSetId = normalizeWorkspaceIdentifier(tab.changeSetId);
        return [
          {
            ...tabWithoutOriginProjectId,
            originSessionId,
            ...projectOriginProps(originProjectId),
            ...(normalizedChangeSetId ? { changeSetId: normalizedChangeSetId } : {}),
            filePath: normalizeWorkspacePath(tab.filePath),
          },
        ];
      });
    const activeTabId = tabs.some((tab) => tab.id === pane.activeTabId)
      ? pane.activeTabId
      : (tabs[0]?.id ?? null);
    const activeSessionId =
      pane.activeSessionId && availableSessionIds.has(pane.activeSessionId)
        ? pane.activeSessionId
        : null;

    return syncPaneState({
      ...pane,
      tabs,
      activeTabId,
      activeSessionId,
    });
  });

  let root = pruneWorkspaceNode(current.root, new Set(panes.map((pane) => pane.id)));

  if (!root && panes.length > 0) {
    root = {
      type: "pane",
      paneId: panes[0].id,
    };
  }

  if (!root && sessions.length > 0) {
    const initialPane = createPane(createSessionTab(sessions[0].id));
    panes = [initialPane];
    root = {
      type: "pane",
      paneId: initialPane.id,
    };
  }

  if (!root) {
    return {
      root: null,
      panes: [],
      activePaneId: null,
    };
  }

  const activePaneId = panes.some((pane) => pane.id === current.activePaneId)
    ? current.activePaneId
    : (panes[0]?.id ?? null);

  return {
    root,
    panes,
    activePaneId,
  };
}

export function findWorkspacePaneIdForSession(workspace: WorkspaceState, sessionId: string) {
  return findSessionTab(workspace, sessionId)?.paneId ?? null;
}

export function openSessionInWorkspaceState(
  workspace: WorkspaceState,
  sessionId: string,
  preferredPaneId: string | null,
  tabIndex?: number,
): WorkspaceState {
  const { targetPaneId, splitAnchorPaneId } = resolveSessionOpenTargetPaneId(
    workspace,
    preferredPaneId,
  );
  const existing = findSessionTab(workspace, sessionId);
  if (existing) {
    if (targetPaneId && (existing.paneId !== targetPaneId || tabIndex !== undefined)) {
      return activatePane(
        moveWorkspaceTabToPane(workspace, existing.paneId, existing.tab.id, targetPaneId, tabIndex),
        targetPaneId,
        existing.tab.id,
      );
    }

    return activatePane(workspace, existing.paneId, existing.tab.id);
  }

  if (splitAnchorPaneId) {
    return openTabInAdjacentPane(
      workspace,
      splitAnchorPaneId,
      createSessionTab(sessionId),
      "row",
      false,
    );
  }

  return openTabInWorkspaceState(
    workspace,
    createSessionTab(sessionId),
    targetPaneId ?? preferredPaneId,
    tabIndex,
  );
}

export function placeSessionDropInWorkspaceState(
  workspace: WorkspaceState,
  sessionId: string,
  targetPaneId: string,
  placement: TabDropPlacement,
  tabIndex?: number,
): WorkspaceState {
  if (placement === "tabs") {
    const existing = findSessionTab(workspace, sessionId);
    if (existing) {
      return activatePane(
        moveWorkspaceTabToPane(workspace, existing.paneId, existing.tab.id, targetPaneId, tabIndex),
        targetPaneId,
        existing.tab.id,
      );
    }

    return openTabInWorkspaceState(
      workspace,
      createSessionTab(sessionId),
      targetPaneId,
      tabIndex,
    );
  }

  if (findWorkspacePaneIdForSession(workspace, sessionId)) {
    return openSessionInWorkspaceState(workspace, sessionId, targetPaneId);
  }

  return placeExternalTab(
    workspace,
    createSessionTab(sessionId),
    targetPaneId,
    placement,
    tabIndex,
  );
}

export function openSourceInWorkspaceState(
  workspace: WorkspaceState,
  path: string | null,
  preferredPaneId: string | null,
  originSessionId: string | null,
  originProjectIdOrOptions: string | null | OpenSourceTabOptions = null,
  options?: OpenSourceTabOptions,
): WorkspaceState {
  const originProjectId =
    typeof originProjectIdOrOptions === "string" || originProjectIdOrOptions === null
      ? originProjectIdOrOptions
      : null;
  const resolvedOptions =
    typeof originProjectIdOrOptions === "string" || originProjectIdOrOptions === null
      ? options
      : originProjectIdOrOptions;
  const normalizedPath = normalizeWorkspacePath(path);
  const targetPaneId = findContextualTargetPaneId(
    workspace,
    preferredPaneId,
    originSessionId,
    "source",
  );
  const focus = createOpenSourceFocus(resolvedOptions);
  const nextTab = createSourceTab(normalizedPath, originSessionId, originProjectId, focus);
  const controlSurfaceTarget = resolveControlSurfaceViewerOpenTarget(workspace, preferredPaneId, "source");
  if (resolvedOptions?.openInNewTab) {
    if (controlSurfaceTarget.splitAnchorPaneId) {
      return openTabInAdjacentPane(workspace, controlSurfaceTarget.splitAnchorPaneId, nextTab, "row", false);
    }
    if (controlSurfaceTarget.targetPaneId) {
      return openTabInWorkspaceState(workspace, nextTab, controlSurfaceTarget.targetPaneId);
    }
    return openContextualTabInWorkspaceState(workspace, nextTab, null, preferredPaneId, originSessionId);
  }

  if (normalizedPath) {
    const existing = findSourceTab(workspace, normalizedPath);
    if (existing) {
      const activatedWorkspace =
        targetPaneId && existing.paneId !== targetPaneId
          ? activatePane(
              moveWorkspaceTabToPane(workspace, existing.paneId, existing.tab.id, targetPaneId),
              targetPaneId,
              existing.tab.id,
            )
          : activatePane(workspace, existing.paneId, existing.tab.id);
      return setSourceTabFocus(activatedWorkspace, existing.tab.id, focus);
    }
  }

  if (targetPaneId) {
    return openTabInWorkspaceState(workspace, nextTab, targetPaneId);
  }

  if (controlSurfaceTarget.splitAnchorPaneId) {
    return openTabInAdjacentPane(workspace, controlSurfaceTarget.splitAnchorPaneId, nextTab, "row", false);
  }
  if (controlSurfaceTarget.targetPaneId) {
    return openTabInWorkspaceState(workspace, nextTab, controlSurfaceTarget.targetPaneId);
  }

  return openContextualTabInWorkspaceState(workspace, nextTab, null, preferredPaneId, originSessionId);
}

export function openFilesystemInWorkspaceState(
  workspace: WorkspaceState,
  rootPath: string | null,
  preferredPaneId: string | null,
  originSessionId: string | null,
  originProjectId: string | null = null,
): WorkspaceState {
  const normalizedRootPath = normalizeWorkspacePath(rootPath);
  if (normalizedRootPath) {
    const existing = findFilesystemTab(workspace, normalizedRootPath);
    if (existing) {
      return activatePane(workspace, existing.paneId, existing.tab.id);
    }
  }

  return openTabInWorkspaceState(
    workspace,
    createFilesystemTab(normalizedRootPath, originSessionId, originProjectId),
    preferredPaneId,
  );
}

export function openGitStatusInWorkspaceState(
  workspace: WorkspaceState,
  workdir: string | null,
  preferredPaneId: string | null,
  originSessionId: string | null,
  originProjectId: string | null = null,
): WorkspaceState {
  const normalizedWorkdir = normalizeWorkspacePath(workdir);
  if (normalizedWorkdir) {
    const existing = findGitStatusTab(workspace, normalizedWorkdir);
    if (existing) {
      return activatePane(workspace, existing.paneId, existing.tab.id);
    }
  }

  return openTabInWorkspaceState(
    workspace,
    createGitStatusTab(normalizedWorkdir, originSessionId, originProjectId),
    preferredPaneId,
  );
}

export function openTerminalInWorkspaceState(
  workspace: WorkspaceState,
  workdir: string | null,
  preferredPaneId: string | null,
  originSessionId: string | null,
  originProjectId: string | null = null,
): WorkspaceState {
  const normalizedWorkdir = normalizeWorkspacePath(workdir);
  const normalizedOriginSessionId = normalizeWorkspaceIdentifier(originSessionId);
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);
  if (normalizedWorkdir) {
    const existing = findTerminalTab(
      workspace,
      normalizedWorkdir,
      normalizedOriginSessionId,
      normalizedOriginProjectId,
    );
    if (existing) {
      return activatePane(workspace, existing.paneId, existing.tab.id);
    }
  }

  return openTabInWorkspaceState(
    workspace,
    createTerminalTab(normalizedWorkdir, normalizedOriginSessionId, normalizedOriginProjectId),
    preferredPaneId,
  );
}

export function openControlPanelInWorkspaceState(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
  originProjectId: string | null = null,
): WorkspaceState {
  const existing = findControlPanelTab(workspace);
  if (existing) {
    return activatePane(workspace, existing.paneId, existing.tab.id);
  }

  return openTabInWorkspaceState(
    workspace,
    createControlPanelTab(originSessionId, originProjectId),
    preferredPaneId,
  );
}

export function openCanvasInWorkspaceState(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
  originProjectId: string | null = null,
): WorkspaceState {
  const targetPaneId = resolveCanvasOpenTargetPaneId(
    workspace,
    preferredPaneId,
    originSessionId,
  );
  const existing = findCanvasTab(workspace);
  if (existing) {
    const nextTab =
      originSessionId !== null || originProjectId !== null
        ? ({
            ...existing.tab,
            originSessionId,
            ...projectOriginProps(normalizeWorkspaceIdentifier(originProjectId)),
          } satisfies WorkspaceCanvasTab)
        : existing.tab;
    const movedWorkspace =
      targetPaneId && existing.paneId !== targetPaneId
        ? moveWorkspaceTabToPane(workspace, existing.paneId, existing.tab.id, targetPaneId)
        : workspace;
    const nextPaneId =
      targetPaneId && movedWorkspace.panes.some((pane) => pane.id === targetPaneId)
        ? targetPaneId
        : existing.paneId;
    const updatedWorkspace =
      nextTab === existing.tab
        ? movedWorkspace
        : replaceWorkspaceTabInPane(movedWorkspace, nextPaneId, existing.tab.id, nextTab);

    return activatePane(updatedWorkspace, nextPaneId, existing.tab.id);
  }

  return openTabInWorkspaceState(
    workspace,
    createCanvasTab(originSessionId, originProjectId),
    targetPaneId ?? preferredPaneId,
  );
}

export function openOrchestratorListInWorkspaceState(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
  originProjectId: string | null = null,
): WorkspaceState {
  const targetPaneId = findContextualTargetPaneId(
    workspace,
    preferredPaneId,
    originSessionId,
    "orchestratorList",
  );
  const existing = findOrchestratorListTab(workspace);
  if (existing) {
    const nextTab =
      originSessionId !== null || originProjectId !== null
        ? ({
            ...existing.tab,
            originSessionId,
            ...projectOriginProps(normalizeWorkspaceIdentifier(originProjectId)),
          } satisfies WorkspaceOrchestratorListTab)
        : existing.tab;
    const movedWorkspace =
      targetPaneId && existing.paneId !== targetPaneId
        ? moveWorkspaceTabToPane(workspace, existing.paneId, existing.tab.id, targetPaneId)
        : workspace;
    const nextPaneId =
      targetPaneId && movedWorkspace.panes.some((pane) => pane.id === targetPaneId)
        ? targetPaneId
        : existing.paneId;
    const updatedWorkspace =
      nextTab === existing.tab
        ? movedWorkspace
        : replaceWorkspaceTabInPane(movedWorkspace, nextPaneId, existing.tab.id, nextTab);

    return activatePane(updatedWorkspace, nextPaneId, existing.tab.id);
  }

  const nextTab = createOrchestratorListTab(originSessionId, originProjectId);
  if (targetPaneId) {
    return openTabInWorkspaceState(workspace, nextTab, targetPaneId);
  }

  const splitAnchorPaneId = (() => {
    if (!preferredPaneId) {
      return preferredPaneId;
    }

    const preferredPane = workspace.panes.find((pane) => pane.id === preferredPaneId);
    if (preferredPane && paneContainsControlPanel(preferredPane)) {
      return findNonControlPanelPaneId(workspace, preferredPane.id) ?? preferredPaneId;
    }

    return preferredPaneId;
  })();

  return openContextualTabInWorkspaceState(
    workspace,
    nextTab,
    null,
    splitAnchorPaneId,
    originSessionId,
  );
}

export function openOrchestratorCanvasInWorkspaceState(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
  originProjectId: string | null = null,
  options: {
    startMode?: "new" | null;
    templateId?: string | null;
  } = {},
): WorkspaceState {
  const targetPaneId = resolveCanvasOpenTargetPaneId(
    workspace,
    preferredPaneId,
    originSessionId,
  );
  return openTabInWorkspaceState(
    workspace,
    createOrchestratorCanvasTab(
      originSessionId,
      originProjectId,
      options.templateId ?? null,
      options.startMode ?? null,
    ),
    targetPaneId ?? preferredPaneId,
  );
}

export function openSessionListInWorkspaceState(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
  originProjectId: string | null = null,
): WorkspaceState {
  const targetPaneId = findContextualTargetPaneId(
    workspace,
    preferredPaneId,
    originSessionId,
    "sessionList",
  );
  const existing = findSessionListTab(workspace);
  if (existing) {
    const nextTab =
      originSessionId !== null || originProjectId !== null
        ? ({
            ...existing.tab,
            originSessionId,
            ...projectOriginProps(normalizeWorkspaceIdentifier(originProjectId)),
          } satisfies WorkspaceSessionListTab)
        : existing.tab;
    const movedWorkspace =
      targetPaneId && existing.paneId !== targetPaneId
        ? moveWorkspaceTabToPane(workspace, existing.paneId, existing.tab.id, targetPaneId)
        : workspace;
    const nextPaneId =
      targetPaneId && movedWorkspace.panes.some((pane) => pane.id === targetPaneId)
        ? targetPaneId
        : existing.paneId;
    const updatedWorkspace =
      nextTab === existing.tab
        ? movedWorkspace
        : replaceWorkspaceTabInPane(movedWorkspace, nextPaneId, existing.tab.id, nextTab);

    return activatePane(updatedWorkspace, nextPaneId, existing.tab.id);
  }

  const nextTab = createSessionListTab(originSessionId, originProjectId);
  if (targetPaneId) {
    return openTabInWorkspaceState(workspace, nextTab, targetPaneId);
  }

  // When opening from a control panel pane, split or reuse the related content pane
  // instead of anchoring the new sessions tab at the workspace edge.
  const splitAnchorPaneId = (() => {
    if (!preferredPaneId) {
      return preferredPaneId;
    }

    const preferredPane = workspace.panes.find((pane) => pane.id === preferredPaneId);
    if (preferredPane && paneContainsControlPanel(preferredPane)) {
      return findNonControlPanelPaneId(workspace, preferredPane.id) ?? preferredPaneId;
    }

    return preferredPaneId;
  })();

  return openContextualTabInWorkspaceState(
    workspace,
    nextTab,
    null,
    splitAnchorPaneId,
    originSessionId,
  );
}

export function openProjectListInWorkspaceState(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
  originProjectId: string | null = null,
): WorkspaceState {
  const existing = findProjectListTab(workspace);
  if (existing) {
    return activatePane(workspace, existing.paneId, existing.tab.id);
  }

  return openTabInWorkspaceState(
    workspace,
    createProjectListTab(originSessionId, originProjectId),
    preferredPaneId,
  );
}

export function openInstructionDebuggerInWorkspaceState(
  workspace: WorkspaceState,
  workdir: string | null,
  preferredPaneId: string | null,
  originSessionId: string | null,
  originProjectId: string | null = null,
): WorkspaceState {
  const normalizedWorkdir = normalizeWorkspacePath(workdir);
  const existing = findInstructionDebuggerTab(workspace, normalizedWorkdir, originSessionId);
  if (existing) {
    return activatePane(workspace, existing.paneId, existing.tab.id);
  }

  return openTabInWorkspaceState(
    workspace,
    createInstructionDebuggerTab(normalizedWorkdir, originSessionId, originProjectId),
    preferredPaneId,
  );
}

export function ensureControlPanelInWorkspaceState(workspace: WorkspaceState): WorkspaceState {
  if (findControlPanelTab(workspace)) {
    return workspace;
  }

  return openTabInWorkspaceState(
    workspace,
    createControlPanelTab(null, null),
    findDefaultControlPanelAnchorPaneId(workspace),
  );
}

export function dockControlPanelAtWorkspaceEdge(
  workspace: WorkspaceState,
  side: "left" | "right",
  preferredControlPanelWidthRatio: number | null = null,
): WorkspaceState {
  const controlPanel = findControlPanelTab(workspace);
  if (!controlPanel || !workspace.root) {
    return workspace;
  }

  const contentRoot = removePaneFromTree(workspace.root, controlPanel.paneId);
  if (!contentRoot) {
    return workspace;
  }

  const controlPanelNode: WorkspaceNode = {
    type: "pane",
    paneId: controlPanel.paneId,
  };
  const rootSplit =
    workspace.root.type === "split" &&
    workspace.root.direction === "row" &&
    (isPaneNode(workspace.root.first, controlPanel.paneId) ||
      isPaneNode(workspace.root.second, controlPanel.paneId))
      ? workspace.root
      : null;
  const controlPanelWidthRatio =
    preferredControlPanelWidthRatio ??
    getDockedControlPanelWidthRatio(workspace.root, controlPanel.paneId) ??
    DEFAULT_CONTROL_PANEL_DOCK_WIDTH_RATIO;
  const nextRatio = side === "left"
    ? controlPanelWidthRatio
    : 1 - controlPanelWidthRatio;

  return {
    ...workspace,
    root: rootSplit
      ? {
          ...rootSplit,
          ratio: nextRatio,
          first: side === "left" ? controlPanelNode : contentRoot,
          second: side === "left" ? contentRoot : controlPanelNode,
        }
      : {
          id: crypto.randomUUID(),
          type: "split",
          direction: "row",
          ratio: nextRatio,
          first: side === "left" ? controlPanelNode : contentRoot,
          second: side === "left" ? contentRoot : controlPanelNode,
        },
  };
}

export function openDiffPreviewInWorkspaceState(
  workspace: WorkspaceState,
  tab: {
    changeType: DiffMessage["changeType"];
    changeSetId?: string | null;
    diff: string;
    documentEnrichmentNote?: string | null;
    documentContent?: GitDiffDocumentContent | null;
    diffMessageId: string;
    filePath: string | null;
    gitSectionId?: GitDiffSection | null;
    language?: string | null;
    originSessionId: string | null;
    originProjectId?: string | null;
    summary: string;
    gitDiffRequestKey?: string | null;
    gitDiffRequest?: GitDiffRequestPayload | null;
    isLoading?: boolean;
    loadError?: string | null;
  },
  preferredPaneId: string | null,
  options?: {
    openInNewTab?: boolean;
    reuseActiveViewerTab?: boolean;
  },
): WorkspaceState {
  const nextTab = createDiffPreviewTab(tab);
  const controlSurfaceTarget = resolveControlSurfaceViewerOpenTarget(workspace, preferredPaneId, "diffPreview");
  if (options?.openInNewTab) {
    if (controlSurfaceTarget.splitAnchorPaneId) {
      return openTabInAdjacentPane(workspace, controlSurfaceTarget.splitAnchorPaneId, nextTab, "row", false);
    }
    if (controlSurfaceTarget.targetPaneId) {
      return openTabInWorkspaceState(workspace, nextTab, controlSurfaceTarget.targetPaneId);
    }
    return openContextualTabInWorkspaceState(
      workspace,
      nextTab,
      null,
      preferredPaneId,
      tab.originSessionId,
    );
  }

  const existing = findDiffPreviewTab(
    workspace,
    normalizeWorkspaceIdentifier(tab.changeSetId),
    tab.diffMessageId,
    tab.originSessionId,
    tab.originProjectId ?? null,
  );
  if (existing) {
    return activatePane(workspace, existing.paneId, existing.tab.id);
  }

  if (options?.reuseActiveViewerTab) {
    if (controlSurfaceTarget.splitAnchorPaneId) {
      return openTabInAdjacentPane(workspace, controlSurfaceTarget.splitAnchorPaneId, nextTab, "row", false);
    }
    if (controlSurfaceTarget.targetPaneId) {
      return replaceActiveViewerTabInPane(workspace, controlSurfaceTarget.targetPaneId, nextTab);
    }

    const contextSessionId =
      tab.originSessionId ??
      (preferredPaneId
        ? (workspace.panes.find((pane) => pane.id === preferredPaneId)?.activeSessionId ?? null)
        : null);
    const targetPaneId =
      findRelatedDiffPreviewPaneId(workspace, preferredPaneId, contextSessionId) ??
      findRelatedViewerPaneId(workspace, preferredPaneId, contextSessionId);
    if (targetPaneId) {
      return replaceActiveViewerTabInPane(workspace, targetPaneId, nextTab);
    }
  }

  if (controlSurfaceTarget.splitAnchorPaneId) {
    return openTabInAdjacentPane(workspace, controlSurfaceTarget.splitAnchorPaneId, nextTab, "row", false);
  }
  if (controlSurfaceTarget.targetPaneId) {
    return replaceActiveViewerTabInPane(workspace, controlSurfaceTarget.targetPaneId, nextTab);
  }

  return openContextualTabInWorkspaceState(
    workspace,
    nextTab,
    null,
    preferredPaneId,
    tab.originSessionId,
  );
}

export function activatePane(
  workspace: WorkspaceState,
  paneId: string,
  tabId?: string | null,
): WorkspaceState {
  const targetPane = workspace.panes.find((pane) => pane.id === paneId) ?? null;
  const targetActiveTabId =
    targetPane &&
    (tabId && targetPane.tabs.some((tab) => tab.id === tabId)
      ? tabId
      : (targetPane.activeTabId ?? targetPane.tabs[0]?.id ?? null));

  if (
    targetPane &&
    workspace.activePaneId === paneId &&
    targetPane.activeTabId === targetActiveTabId
  ) {
    return workspace;
  }

  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      const activeTabId =
        tabId && pane.tabs.some((tab) => tab.id === tabId)
          ? tabId
          : (pane.activeTabId ?? pane.tabs[0]?.id ?? null);

      return syncPaneState({
        ...pane,
        activeTabId,
      });
    }),
    activePaneId: paneId,
  };
}

export function closeWorkspaceTab(
  workspace: WorkspaceState,
  paneId: string,
  tabId: string,
): WorkspaceState {
  const pane = workspace.panes.find((candidate) => candidate.id === paneId);
  if (!pane) {
    return workspace;
  }

  const closedTabIndex = pane.tabs.findIndex((candidate) => candidate.id === tabId);
  const nextTabs = pane.tabs.filter((candidate) => candidate.id !== tabId);
  if (nextTabs.length === 0) {
    const panes = workspace.panes.filter((candidate) => candidate.id !== paneId);
    const root = removePaneFromTree(workspace.root, paneId);

    return {
      root,
      panes,
      activePaneId:
        panes.some((candidate) => candidate.id === workspace.activePaneId)
          ? workspace.activePaneId
          : (panes[0]?.id ?? null),
    };
  }

  return {
    ...workspace,
    panes: workspace.panes.map((candidate) => {
      if (candidate.id !== paneId) {
        return candidate;
      }

      return syncPaneState({
        ...candidate,
        tabs: nextTabs,
        activeTabId:
          candidate.activeTabId === tabId
            ? (nextTabs[Math.min(Math.max(closedTabIndex, 0), nextTabs.length - 1)]?.id ?? null)
            : candidate.activeTabId,
      });
    }),
    activePaneId: paneId,
  };
}

export function splitPane(
  workspace: WorkspaceState,
  paneId: string,
  direction: "row" | "column",
): WorkspaceState {
  const pane = workspace.panes.find((candidate) => candidate.id === paneId);
  if (!pane || !workspace.root) {
    return workspace;
  }

  const activeTab = pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? null;
  const tabToMove = pane.tabs.length > 1 ? activeTab : null;
  const newPane = createPane(tabToMove, pane.lastSessionViewMode);
  const panes = workspace.panes.map((candidate) => {
    if (candidate.id !== paneId || !tabToMove) {
      return candidate;
    }

    return syncPaneState({
      ...candidate,
      tabs: candidate.tabs.filter((tab) => tab.id !== tabToMove.id),
      activeTabId:
        candidate.activeTabId === tabToMove.id
          ? (candidate.tabs.find((tab) => tab.id !== tabToMove.id)?.id ?? null)
          : candidate.activeTabId,
    });
  });

  return {
    root: insertPaneAdjacent(workspace.root, paneId, direction, newPane.id, false),
    panes: [...panes, newPane],
    activePaneId: newPane.id,
  };
}

export function placeDraggedTab(
  workspace: WorkspaceState,
  sourcePaneId: string,
  tabId: string,
  targetPaneId: string,
  placement: TabDropPlacement,
  tabIndex?: number,
): WorkspaceState {
  const sourcePane = workspace.panes.find((pane) => pane.id === sourcePaneId);
  const targetPane = workspace.panes.find((pane) => pane.id === targetPaneId);
  const draggedTab = sourcePane?.tabs.find((tab) => tab.id === tabId);
  if (!sourcePane || !targetPane || !draggedTab) {
    return workspace;
  }

  if (!isAllowedControlPanelPlacement(targetPane, draggedTab, placement)) {
    return workspace;
  }

  if (placement === "tabs") {
    const requestedTabIndex = tabIndex ?? targetPane.tabs.length;
    if (sourcePaneId === targetPaneId) {
      const sourceTabIndex = sourcePane.tabs.findIndex((tab) => tab.id === tabId);
      const adjustedTabIndex =
        sourceTabIndex >= 0 && requestedTabIndex > sourceTabIndex
          ? requestedTabIndex - 1
          : requestedTabIndex;

      return addWorkspaceTabToPane(workspace, targetPaneId, draggedTab, adjustedTabIndex);
    }

    const withoutSource = closeWorkspaceTab(workspace, sourcePaneId, tabId);
    return addWorkspaceTabToPane(withoutSource, targetPaneId, draggedTab, requestedTabIndex);
  }

  if (sourcePaneId === targetPaneId && sourcePane.tabs.length <= 1) {
    return workspace;
  }

  const withoutSource = closeWorkspaceTab(workspace, sourcePaneId, tabId);
  if (!withoutSource.root || !withoutSource.panes.some((pane) => pane.id === targetPaneId)) {
    return workspace;
  }

  const newPane = createPane(draggedTab, targetPane.lastSessionViewMode);
  const direction = placement === "left" || placement === "right" ? "row" : "column";
  const placeBefore = placement === "left" || placement === "top";

  return {
    root: insertPaneAdjacent(withoutSource.root, targetPaneId, direction, newPane.id, placeBefore),
    panes: [...withoutSource.panes, newPane],
    activePaneId: newPane.id,
  };
}

export function placeExternalTab(
  workspace: WorkspaceState,
  tab: WorkspaceTab,
  targetPaneId: string,
  placement: TabDropPlacement,
  tabIndex?: number,
): WorkspaceState {
  const transferredTab = cloneWorkspaceTab(tab);

  if (placement === "tabs") {
    return openTabInWorkspaceState(workspace, transferredTab, targetPaneId, tabIndex);
  }

  const targetPane = workspace.panes.find((pane) => pane.id === targetPaneId);
  if (!targetPane || !workspace.root) {
    return openTabInWorkspaceState(workspace, transferredTab, targetPaneId, tabIndex);
  }

  if (!isAllowedControlPanelPlacement(targetPane, transferredTab, placement)) {
    return workspace;
  }

  const newPane = createPane(transferredTab, targetPane.lastSessionViewMode);
  const direction = placement === "left" || placement === "right" ? "row" : "column";
  const placeBefore = placement === "left" || placement === "top";

  return {
    root: insertPaneAdjacent(workspace.root, targetPaneId, direction, newPane.id, placeBefore),
    panes: [...workspace.panes, newPane],
    activePaneId: newPane.id,
  };
}

export function updateSplitRatio(
  workspace: WorkspaceState,
  splitId: string,
  ratio: number,
): WorkspaceState {
  if (!workspace.root) {
    return workspace;
  }

  return {
    ...workspace,
    root: updateSplitRatioInNode(workspace.root, splitId, ratio),
  };
}

export function createPane(
  initialTab?: WorkspaceTab | null,
  sessionViewMode: SessionPaneViewMode = "session",
): WorkspacePane {
  return syncPaneState({
    id: crypto.randomUUID(),
    tabs: initialTab ? [initialTab] : [],
    activeTabId: initialTab?.id ?? null,
    activeSessionId: null,
    viewMode: sessionViewMode,
    lastSessionViewMode: sessionViewMode,
    sourcePath: null,
  });
}

export function upsertCanvasSessionCard(
  workspace: WorkspaceState,
  canvasTabId: string,
  card: WorkspaceCanvasCard,
): WorkspaceState {
  const normalizedCard = normalizeWorkspaceCanvasCard(card);
  if (!normalizedCard) {
    return workspace;
  }

  return updateCanvasTab(workspace, canvasTabId, (tab) => {
    const existingCardIndex = tab.cards.findIndex(
      (candidate) => candidate.sessionId === normalizedCard.sessionId,
    );
    if (existingCardIndex < 0) {
      return {
        ...tab,
        cards: [...tab.cards, normalizedCard],
      };
    }

    const existingCard = tab.cards[existingCardIndex];
    if (
      existingCard.x === normalizedCard.x &&
      existingCard.y === normalizedCard.y
    ) {
      return tab;
    }

    return {
      ...tab,
      cards: tab.cards.map((candidate, index) =>
        index === existingCardIndex ? normalizedCard : candidate,
      ),
    };
  });
}

export function removeCanvasSessionCard(
  workspace: WorkspaceState,
  canvasTabId: string,
  sessionId: string,
): WorkspaceState {
  const normalizedSessionId = normalizeWorkspaceIdentifier(sessionId);
  if (!normalizedSessionId) {
    return workspace;
  }

  return updateCanvasTab(workspace, canvasTabId, (tab) => {
    const cards = tab.cards.filter((card) => card.sessionId !== normalizedSessionId);
    return cards.length === tab.cards.length
      ? tab
      : {
          ...tab,
          cards,
        };
  });
}

export function setCanvasZoom(
  workspace: WorkspaceState,
  canvasTabId: string,
  zoom: number,
): WorkspaceState {
  const normalizedZoom = normalizeWorkspaceCanvasZoom(zoom);

  return updateCanvasTab(workspace, canvasTabId, (tab) => {
    if (normalizeWorkspaceCanvasZoom(tab.zoom) === normalizedZoom) {
      return tab;
    }

    const { zoom: _ignoredZoom, ...tabWithoutZoom } = tab;
    return {
      ...tabWithoutZoom,
      ...canvasZoomProps(normalizedZoom),
    };
  });
}

export function setPaneViewMode(
  workspace: WorkspaceState,
  paneId: string,
  viewMode: SessionPaneViewMode,
): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId || !isSessionTabActive(pane)) {
        return pane;
      }

      return syncPaneState({
        ...pane,
        viewMode,
        lastSessionViewMode: viewMode,
      });
    }),
  };
}

export function setPaneSourcePath(
  workspace: WorkspaceState,
  paneId: string,
  sourcePath: string,
): WorkspaceState {
  const nextPath = normalizeWorkspacePath(sourcePath);
  const currentPane = workspace.panes.find((pane) => pane.id === paneId);
  const activeSourceTabId =
    currentPane?.tabs.find(
      (tab): tab is WorkspaceSourceTab => tab.id === currentPane.activeTabId && tab.kind === "source",
    )?.id ?? null;
  const existing = nextPath ? findSourceTab(workspace, nextPath) : null;
  if (existing && existing.tab.id !== activeSourceTabId) {
    return activatePane(
      setSourceTabFocus(workspace, existing.tab.id, EMPTY_WORKSPACE_SOURCE_FOCUS),
      existing.paneId,
      existing.tab.id,
    );
  }

  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      const activeTab = pane.tabs.find((tab) => tab.id === pane.activeTabId);
      if (!activeTab || activeTab.kind !== "source") {
        return pane;
      }

      return syncPaneState({
        ...pane,
        tabs: pane.tabs.map((tab) => {
          if (tab.id !== activeTab.id || tab.kind !== "source") {
            return tab;
          }

          const {
            originProjectId: _ignoredOriginProjectId,
            focusLineNumber: _ignoredFocusLineNumber,
            focusColumnNumber: _ignoredFocusColumnNumber,
            focusToken: _ignoredFocusToken,
            ...tabWithoutOriginProjectId
          } = tab;
          return {
            ...tabWithoutOriginProjectId,
            path: nextPath,
            originSessionId: activeTab.originSessionId ?? pane.activeSessionId ?? null,
            ...projectOriginProps(activeTab.originProjectId ?? null),
          };
        }),
      });
    }),
  };
}

export function addWorkspaceTabToPane(
  workspace: WorkspaceState,
  paneId: string,
  tab: WorkspaceTab,
  tabIndex?: number,
): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      return syncPaneState({
        ...pane,
        tabs: insertTabAtIndex(pane.tabs, tab, tabIndex ?? pane.tabs.length),
        activeTabId: tab.id,
      });
    }),
    activePaneId: paneId,
  };
}

function replaceActiveViewerTabInPane(
  workspace: WorkspaceState,
  paneId: string,
  tab: WorkspaceTab,
): WorkspaceState {
  const pane = workspace.panes.find((candidate) => candidate.id === paneId) ?? null;
  const activeTab = pane ? getActiveTab(pane) : null;
  if (!pane || !activeTab || (activeTab.kind !== "source" && activeTab.kind !== "diffPreview")) {
    return addWorkspaceTabToPane(workspace, paneId, tab);
  }

  return {
    ...workspace,
    panes: workspace.panes.map((candidate) => {
      if (candidate.id !== paneId) {
        return candidate;
      }

      return syncPaneState({
        ...candidate,
        tabs: candidate.tabs.map((entry) => (entry.id === activeTab.id ? tab : entry)),
        activeTabId: tab.id,
      });
    }),
    activePaneId: paneId,
  };
}

function moveWorkspaceTabToPane(
  workspace: WorkspaceState,
  sourcePaneId: string,
  tabId: string,
  targetPaneId: string,
  tabIndex?: number,
) {
  const sourcePane = workspace.panes.find((pane) => pane.id === sourcePaneId);
  const targetPane = workspace.panes.find((pane) => pane.id === targetPaneId);
  const tab = sourcePane?.tabs.find((candidate) => candidate.id === tabId);
  if (!sourcePane || !targetPane || !tab) {
    return workspace;
  }

  if (sourcePaneId === targetPaneId) {
    if (tabIndex === undefined) {
      return activatePane(workspace, sourcePaneId, tabId);
    }

    const sourceTabIndex = sourcePane.tabs.findIndex((candidate) => candidate.id === tabId);
    const adjustedTabIndex =
      sourceTabIndex >= 0 && tabIndex > sourceTabIndex ? tabIndex - 1 : tabIndex;
    return addWorkspaceTabToPane(workspace, sourcePaneId, tab, adjustedTabIndex);
  }

  const withoutSource = closeWorkspaceTab(workspace, sourcePaneId, tabId);
  if (!withoutSource.panes.some((pane) => pane.id === targetPaneId)) {
    return workspace;
  }

  return addWorkspaceTabToPane(withoutSource, targetPaneId, tab, tabIndex);
}

export function updateGitDiffPreviewTabInWorkspaceState(
  workspace: WorkspaceState,
  requestKey: string,
  update: (tab: WorkspaceDiffPreviewTab) => WorkspaceDiffPreviewTab,
): WorkspaceState {
  let changed = false;
  const panes = workspace.panes.map((pane) => {
    let paneChanged = false;
    const tabs = pane.tabs.map((tab) => {
      if (tab.kind !== "diffPreview" || tab.gitDiffRequestKey !== requestKey) {
        return tab;
      }
      const nextTab = update(tab);
      if (nextTab === tab) {
        return tab;
      }
      changed = true;
      paneChanged = true;
      return nextTab;
    });
    return paneChanged ? syncPaneState({ ...pane, tabs }) : pane;
  });

  return changed ? { ...workspace, panes } : workspace;
}

export function stripLoadingGitDiffPreviewTabsFromWorkspaceState(workspace: WorkspaceState): WorkspaceState {
  let nextWorkspace = workspace;
  // Git-status preview tabs start as empty loading placeholders. Restored
  // diff tabs can also be loading while documentContent is re-fetched, but
  // they keep durable diff text and must survive persistence during that
  // restore window.
  const loadingTabs = workspace.panes.flatMap((pane) =>
    pane.tabs
      .filter(
        (tab): tab is WorkspaceDiffPreviewTab =>
          tab.kind === "diffPreview" &&
          tab.isLoading === true &&
          Boolean(tab.gitDiffRequestKey) &&
          tab.diff.trim().length === 0,
      )
      .map((tab) => ({ paneId: pane.id, tabId: tab.id })),
  );

  for (const loadingTab of loadingTabs) {
    if (!nextWorkspace.panes.some((pane) => pane.id === loadingTab.paneId)) {
      continue;
    }
    nextWorkspace = closeWorkspaceTab(nextWorkspace, loadingTab.paneId, loadingTab.tabId);
  }

  return nextWorkspace;
}

export function stripDiffPreviewDocumentContentFromWorkspaceState(workspace: WorkspaceState): WorkspaceState {
  let changed = false;
  const panes = workspace.panes.map((pane) => {
    let paneChanged = false;
    const tabs = pane.tabs.map((tab) => {
      if (tab.kind !== "diffPreview" || !tab.documentContent) {
        return tab;
      }

      const { documentContent: _documentContent, ...tabWithoutDocumentContent } = tab;
      changed = true;
      paneChanged = true;
      return tabWithoutDocumentContent;
    });

    return paneChanged ? syncPaneState({ ...pane, tabs }) : pane;
  });

  return changed ? { ...workspace, panes } : workspace;
}

function replaceWorkspaceTabInPane<T extends WorkspaceTab>(
  workspace: WorkspaceState,
  paneId: string,
  tabId: string,
  nextTab: T,
) {
  let changed = false;
  const nextPanes = workspace.panes.map((pane) => {
    if (pane.id !== paneId) {
      return pane;
    }

    const nextTabs = pane.tabs.map((tab) => {
      if (tab.id !== tabId) {
        return tab;
      }

      changed = true;
      return nextTab;
    });
    return changed ? syncPaneState({ ...pane, tabs: nextTabs }) : pane;
  });

  return changed ? { ...workspace, panes: nextPanes } : workspace;
}

export function getSplitRatio(node: WorkspaceNode | null, splitId: string): number | null {
  if (!node || node.type === "pane") {
    return null;
  }

  if (node.id === splitId) {
    return node.ratio;
  }

  return getSplitRatio(node.first, splitId) ?? getSplitRatio(node.second, splitId);
}

function openTabInWorkspaceState(
  workspace: WorkspaceState,
  tab: WorkspaceTab,
  preferredPaneId: string | null,
  tabIndex?: number,
): WorkspaceState {
  const targetPaneId = workspace.panes.some((pane) => pane.id === preferredPaneId)
    ? preferredPaneId
    : (workspace.activePaneId ?? workspace.panes[0]?.id ?? null);

  if (!targetPaneId) {
    const pane = createPane(tab);
    return {
      root: {
        type: "pane",
        paneId: pane.id,
      },
      panes: [pane],
      activePaneId: pane.id,
    };
  }

  const targetPane = workspace.panes.find((pane) => pane.id === targetPaneId) ?? null;
  if (targetPane && tab.kind !== "controlPanel" && paneContainsControlPanel(targetPane)) {
    const alternatePaneId = findNonControlPanelPaneId(workspace, targetPane.id);
    if (alternatePaneId) {
      return addWorkspaceTabToPane(workspace, alternatePaneId, tab, tabIndex);
    }

    return openTabInAdjacentPane(workspace, targetPane.id, tab, "row", false);
  }

  if (targetPane && tab.kind === "controlPanel" && !paneContainsControlPanel(targetPane)) {
    return openTabInAdjacentPane(workspace, targetPane.id, tab, "row", true);
  }

  return addWorkspaceTabToPane(workspace, targetPaneId, tab, tabIndex);
}

function openTabInAdjacentPane(
  workspace: WorkspaceState,
  paneId: string,
  tab: WorkspaceTab,
  direction: "row" | "column",
  placeBefore: boolean,
): WorkspaceState {
  const referencePane = workspace.panes.find((pane) => pane.id === paneId);
  if (!referencePane || !workspace.root) {
    return openTabInWorkspaceState(workspace, tab, paneId);
  }

  const newPane = createPane(tab, referencePane.lastSessionViewMode);
  return {
    root: insertPaneAdjacent(workspace.root, paneId, direction, newPane.id, placeBefore),
    panes: [...workspace.panes, newPane],
    activePaneId: newPane.id,
  };
}

function openContextualTabInWorkspaceState<T extends WorkspaceTab>(
  workspace: WorkspaceState,
  tab: T,
  existing: { paneId: string; tab: T } | null,
  preferredPaneId: string | null,
  originSessionId: string | null,
): WorkspaceState {
  if (existing) {
    return activatePane(workspace, existing.paneId, existing.tab.id);
  }

  const targetPaneId = findContextualTargetPaneId(
    workspace,
    preferredPaneId,
    originSessionId,
    tab.kind,
  );
  if (targetPaneId) {
    return openTabInWorkspaceState(workspace, tab, targetPaneId);
  }

  if (shouldOpenTabInAdjacentPane(workspace, preferredPaneId, tab.kind)) {
    return openTabInAdjacentPane(workspace, preferredPaneId!, tab, "row", false);
  }

  // Too many panes to split - try to find an adjacent content pane to reuse
  // rather than replacing the current pane's content (e.g., don't replace git status with a source tab).
  if (preferredPaneId) {
    const siblingPaneId = findSiblingContentPaneId(workspace, preferredPaneId);
    if (siblingPaneId) {
      return openTabInWorkspaceState(workspace, tab, siblingPaneId);
    }
  }

  return openTabInWorkspaceState(workspace, tab, preferredPaneId);
}

function syncPaneState(pane: WorkspacePane): WorkspacePane {
  const activeTab = pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? pane.tabs[0] ?? null;
  if (!activeTab) {
    return {
      ...pane,
      activeTabId: null,
      activeSessionId: null,
      viewMode: pane.lastSessionViewMode,
      sourcePath: null,
    };
  }

  if (activeTab.kind === "session") {
    const viewMode = pane.viewMode === "source" ? pane.lastSessionViewMode : pane.viewMode;
    const nextSessionViewMode = normalizeSessionViewMode(viewMode, pane.lastSessionViewMode);
    return {
      ...pane,
      activeTabId: activeTab.id,
      activeSessionId: activeTab.sessionId,
      viewMode: nextSessionViewMode,
      lastSessionViewMode: nextSessionViewMode,
      sourcePath: null,
    };
  }

  if (activeTab.kind === "source") {
    return {
      ...pane,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(activeTab.originSessionId, pane.activeSessionId, pane.tabs),
      viewMode: "source",
      sourcePath: activeTab.path,
    };
  }

  if (activeTab.kind === "canvas") {
    return {
      ...pane,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(activeTab.originSessionId, pane.activeSessionId, pane.tabs),
      viewMode: "canvas",
      sourcePath: null,
    };
  }

  if (activeTab.kind === "orchestratorCanvas") {
    return {
      ...pane,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(activeTab.originSessionId, pane.activeSessionId, pane.tabs),
      viewMode: "orchestratorCanvas",
      sourcePath: null,
    };
  }

  if (activeTab.kind === "filesystem") {
    return {
      ...pane,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(activeTab.originSessionId, pane.activeSessionId, pane.tabs),
      viewMode: "filesystem",
      sourcePath: null,
    };
  }

  if (
    activeTab.kind === "controlPanel" ||
    activeTab.kind === "orchestratorList" ||
    activeTab.kind === "sessionList" ||
    activeTab.kind === "projectList"
  ) {
    return syncOriginOnlyPaneState(pane, activeTab);
  }

  if (activeTab.kind === "instructionDebugger") {
    return {
      ...pane,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(activeTab.originSessionId, pane.activeSessionId, pane.tabs),
      viewMode: "instructionDebugger",
      sourcePath: null,
    };
  }

  if (activeTab.kind === "terminal") {
    return {
      ...pane,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(activeTab.originSessionId, pane.activeSessionId, pane.tabs),
      viewMode: "terminal",
      sourcePath: null,
    };
  }

  return {
    ...pane,
    activeTabId: activeTab.id,
    activeSessionId: resolveOriginSessionId(activeTab.originSessionId, pane.activeSessionId, pane.tabs),
    viewMode: activeTab.kind === "gitStatus" ? "gitStatus" : "diffPreview",
    sourcePath: null,
  };
}

function normalizeSessionViewMode(
  viewMode: PaneViewMode,
  fallback: SessionPaneViewMode,
): SessionPaneViewMode {
  return isSessionPaneViewMode(viewMode) ? viewMode : fallback;
}

function firstSessionTabId(tabs: WorkspaceTab[]): string | null {
  return tabs.find((tab): tab is WorkspaceSessionTab => tab.kind === "session")?.sessionId ?? null;
}

function isSessionTabActive(pane: WorkspacePane) {
  return pane.tabs.some((tab) => tab.id === pane.activeTabId && tab.kind === "session");
}

function findSessionTab(workspace: WorkspaceState, sessionId: string) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceSessionTab =>
        candidate.kind === "session" && candidate.sessionId === sessionId,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findSourceTab(workspace: WorkspaceState, path: string) {
  const normalizedPath = normalizeWorkspacePath(path);
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceSourceTab =>
        candidate.kind === "source" &&
        normalizeWorkspacePath(candidate.path) === normalizedPath,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findFilesystemTab(workspace: WorkspaceState, rootPath: string) {
  const normalizedRootPath = normalizeWorkspacePath(rootPath);
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceFilesystemTab =>
        candidate.kind === "filesystem" &&
        normalizeWorkspacePath(candidate.rootPath) === normalizedRootPath,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findGitStatusTab(workspace: WorkspaceState, workdir: string) {
  const normalizedWorkdir = normalizeWorkspacePath(workdir);
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceGitStatusTab =>
        candidate.kind === "gitStatus" &&
        normalizeWorkspacePath(candidate.workdir) === normalizedWorkdir,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findTerminalTab(
  workspace: WorkspaceState,
  workdir: string,
  originSessionId: string | null,
  originProjectId: string | null,
) {
  const normalizedWorkdir = normalizeWorkspacePath(workdir);
  const normalizedOriginSessionId = normalizeWorkspaceIdentifier(originSessionId);
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceTerminalTab =>
        candidate.kind === "terminal" &&
        normalizeWorkspacePath(candidate.workdir) === normalizedWorkdir &&
        normalizeWorkspaceIdentifier(candidate.originSessionId) === normalizedOriginSessionId &&
        normalizeWorkspaceIdentifier(candidate.originProjectId ?? null) === normalizedOriginProjectId,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findControlPanelTab(workspace: WorkspaceState) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceControlPanelTab => candidate.kind === "controlPanel",
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findOrchestratorListTab(workspace: WorkspaceState) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceOrchestratorListTab =>
        candidate.kind === "orchestratorList",
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findCanvasTab(workspace: WorkspaceState) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceCanvasTab => candidate.kind === "canvas",
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findSessionListTab(workspace: WorkspaceState) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceSessionListTab => candidate.kind === "sessionList",
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findProjectListTab(workspace: WorkspaceState) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceProjectListTab => candidate.kind === "projectList",
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function reconcileOriginOnlyTab(
  tab: WorkspaceOriginOnlyTab,
  originSessionId: string | null,
  originProjectId: string | null,
): WorkspaceOriginOnlyTab {
  const { originProjectId: _ignoredOriginProjectId, ...tabWithoutOriginProjectId } = tab;
  return {
    ...tabWithoutOriginProjectId,
    originSessionId,
    ...projectOriginProps(originProjectId),
  };
}

function syncOriginOnlyPaneState(
  pane: WorkspacePane,
  activeTab: WorkspaceOriginOnlyTab,
): WorkspacePane {
  return {
    ...pane,
    activeTabId: activeTab.id,
    activeSessionId: resolveOriginSessionId(activeTab.originSessionId, pane.activeSessionId, pane.tabs),
    viewMode: activeTab.kind,
    sourcePath: null,
  };
}

function findInstructionDebuggerTab(
  workspace: WorkspaceState,
  workdir: string | null,
  originSessionId: string | null,
) {
  const normalizedWorkdir = normalizeWorkspacePath(workdir);
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceInstructionDebuggerTab =>
        candidate.kind === "instructionDebugger" &&
        candidate.originSessionId === originSessionId &&
        normalizeWorkspacePath(candidate.workdir) === normalizedWorkdir,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function isPaneNode(node: WorkspaceNode, paneId: string): boolean {
  return node.type === "pane" && node.paneId === paneId;
}

function getDockedControlPanelWidthRatio(
  root: WorkspaceNode | null,
  controlPanelPaneId: string,
): number | null {
  if (!root || root.type === "pane" || root.direction !== "row") {
    return null;
  }

  if (isPaneNode(root.first, controlPanelPaneId)) {
    return root.ratio;
  }

  if (isPaneNode(root.second, controlPanelPaneId)) {
    return 1 - root.ratio;
  }

  return null;
}

function findDefaultControlPanelAnchorPaneId(workspace: WorkspaceState) {
  return (
    findNonControlPanelPaneId(workspace, null) ??
    workspace.activePaneId ??
    workspace.panes[0]?.id ??
    null
  );
}

function findSiblingContentPaneId(workspace: WorkspaceState, excludePaneId: string) {
  // Find another pane that holds user content (session, source, diff, etc.) and prefer the active pane.
  // Skip all control-surface panes, including standalone Files/Git/Sessions/Projects views.
  const candidates = workspace.panes.filter((pane) => {
    if (pane.id === excludePaneId) {
      return false;
    }
    const activeTab = getActiveTab(pane);
    if (!activeTab) {
      return false;
    }
    return !paneIsControlSurface(pane);
  });
  if (candidates.length === 0) {
    return null;
  }
  // Prefer the workspace's active pane if it's in the candidate list.
  const activePaneCandidate = candidates.find((pane) => pane.id === workspace.activePaneId);
  return activePaneCandidate?.id ?? candidates[0]?.id ?? null;
}

function findNonControlPanelPaneId(workspace: WorkspaceState, excludePaneId: string | null) {
  const activePane =
    workspace.activePaneId && workspace.activePaneId !== excludePaneId
      ? workspace.panes.find((pane) => pane.id === workspace.activePaneId) ?? null
      : null;
  if (activePane && !paneContainsControlPanel(activePane)) {
    return activePane.id;
  }

  const sessionPane = workspace.panes.find((pane) => {
    if (pane.id === excludePaneId || paneContainsControlPanel(pane)) {
      return false;
    }

    return getActiveTab(pane)?.kind === "session";
  });
  if (sessionPane) {
    return sessionPane.id;
  }

  return (
    workspace.panes.find(
      (pane) => pane.id !== excludePaneId && !paneContainsControlPanel(pane),
    )?.id ?? null
  );
}

function findContentPaneId(workspace: WorkspaceState, excludePaneId: string | null) {
  const activePane =
    workspace.activePaneId && workspace.activePaneId !== excludePaneId
      ? workspace.panes.find((pane) => pane.id === workspace.activePaneId) ?? null
      : null;
  if (activePane && !paneIsControlSurface(activePane)) {
    return activePane.id;
  }

  const sessionPane = workspace.panes.find((pane) => {
    if (pane.id === excludePaneId || paneIsControlSurface(pane)) {
      return false;
    }

    return getActiveTab(pane)?.kind === "session";
  });
  if (sessionPane) {
    return sessionPane.id;
  }

  return workspace.panes.find((pane) => pane.id !== excludePaneId && !paneIsControlSurface(pane))?.id ?? null;
}

function resolveSessionOpenTargetPaneId(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
): { targetPaneId: string | null; splitAnchorPaneId: string | null } {
  if (!preferredPaneId) {
    return { targetPaneId: null, splitAnchorPaneId: null };
  }

  const preferredPane = workspace.panes.find((pane) => pane.id === preferredPaneId) ?? null;
  if (!preferredPane) {
    return { targetPaneId: null, splitAnchorPaneId: null };
  }

  if (!paneIsControlSurface(preferredPane)) {
    return { targetPaneId: null, splitAnchorPaneId: null };
  }

  const directionalContentPaneId = findDirectionalContentPaneId(workspace, preferredPane.id);
  const hasSingleControlSurfacePane = workspace.panes.filter(paneIsControlSurface).length === 1;
  const hasSingleContentPane = workspace.panes.filter((pane) => !paneIsControlSurface(pane)).length === 1;
  if (
    directionalContentPaneId &&
    workspace.panes.length === 2 &&
    hasSingleControlSurfacePane &&
    hasSingleContentPane
  ) {
    return { targetPaneId: null, splitAnchorPaneId: directionalContentPaneId };
  }

  return {
    targetPaneId:
      directionalContentPaneId ??
      findSiblingContentPaneId(workspace, preferredPane.id) ??
      findContentPaneId(workspace, preferredPane.id),
    splitAnchorPaneId: null,
  };
}

function resolveControlSurfaceViewerOpenTarget(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  tabKind: "source" | "diffPreview",
): { targetPaneId: string | null; splitAnchorPaneId: string | null } {
  if (!preferredPaneId) {
    return { targetPaneId: null, splitAnchorPaneId: null };
  }

  const preferredPane = workspace.panes.find((pane) => pane.id === preferredPaneId) ?? null;
  if (!preferredPane || !paneIsControlSurface(preferredPane)) {
    return { targetPaneId: null, splitAnchorPaneId: null };
  }

  const contentPaneId =
    findDirectionalContentPaneId(workspace, preferredPane.id) ??
    findSiblingContentPaneId(workspace, preferredPane.id) ??
    findContentPaneId(workspace, preferredPane.id);
  if (!contentPaneId) {
    return { targetPaneId: null, splitAnchorPaneId: null };
  }

  const contentPane = workspace.panes.find((pane) => pane.id === contentPaneId) ?? null;
  const contentActiveTabKind = contentPane ? getActiveTab(contentPane)?.kind : null;
  if (contentActiveTabKind === "source" || contentActiveTabKind === "diffPreview") {
    return { targetPaneId: contentPaneId, splitAnchorPaneId: null };
  }

  if (shouldOpenTabInAdjacentPane(workspace, contentPaneId, tabKind)) {
    return { targetPaneId: null, splitAnchorPaneId: contentPaneId };
  }

  return { targetPaneId: contentPaneId, splitAnchorPaneId: null };
}

function resolveCanvasOpenTargetPaneId(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
) {
  if (!preferredPaneId) {
    return null;
  }

  const preferredPane = workspace.panes.find((pane) => pane.id === preferredPaneId) ?? null;
  if (!preferredPane) {
    return null;
  }

  const preferredActiveTab = getActiveTab(preferredPane);
  if (preferredActiveTab?.kind === "session") {
    return preferredPane.id;
  }

  const originSessionPaneId = originSessionId
    ? findSessionTab(workspace, originSessionId)?.paneId ?? null
    : null;
  if (originSessionPaneId) {
    return originSessionPaneId;
  }

  if (!paneIsControlSurface(preferredPane)) {
    return null;
  }

  return (
    findNearestSessionPaneId(workspace, preferredPane.id) ??
    findSiblingContentPaneId(workspace, preferredPane.id) ??
    findNonControlPanelPaneId(workspace, preferredPane.id)
  );
}
/**
 * Re-scope a control surface pane's active tab to a new session/project context.
 * Updates workdir, originSessionId, and originProjectId on standalone tabs
 * (gitStatus, filesystem) so they reflect the newly-selected session's project.
 */
/**
 * Rescope the active tab of a control-surface pane to a new
 * session/project context. Called when the user activates a pane so that
 * control surfaces auto-follow the ambient session.
 *
 * NOTE: terminal tabs are deliberately NOT rescoped here even though they
 * render inside a `.control-panel-section-stack terminal-section-stack`
 * wrapper in `App.tsx` (which can make them look like control surfaces).
 * A terminal's `originProjectId` is load-bearing for its command history
 * — the history store is keyed on the per-tab UUID, but the routing scope
 * for `runTerminalCommand` comes from `originProjectId` via the
 * `TerminalPanel` `projectId` prop, and silently reassigning that on
 * every pane activation would make history bleed across projects. Users
 * rescope a terminal explicitly via the project-scope dropdown at
 * `App.tsx` (`shouldRenderTerminalProjectScope`); this function should
 * leave terminal tabs alone. See also `CONTROL_SURFACE_KINDS` below,
 * which intentionally excludes `"terminal"` for the same reason.
 */
export function rescopeControlSurfacePane(
  workspace: WorkspaceState,
  controlSurfacePaneId: string,
  sessionId: string | null,
  projectId: string | null,
  workdir: string | null,
): WorkspaceState {
  const paneIndex = workspace.panes.findIndex((pane) => pane.id === controlSurfacePaneId);
  if (paneIndex === -1) {
    return workspace;
  }
  const pane = workspace.panes[paneIndex]!;
  const activeTab = getActiveTab(pane);
  if (!activeTab) {
    return workspace;
  }

  let updatedTab: WorkspaceTab | null = null;

  if (activeTab.kind === "gitStatus" && workdir) {
    updatedTab = { ...activeTab, workdir, originSessionId: sessionId ?? activeTab.originSessionId, originProjectId: projectId };
  } else if (activeTab.kind === "filesystem" && workdir) {
    updatedTab = { ...activeTab, rootPath: workdir, originSessionId: sessionId ?? activeTab.originSessionId, originProjectId: projectId };
  } else if (activeTab.kind === "controlPanel" || activeTab.kind === "sessionList" || activeTab.kind === "projectList" || activeTab.kind === "orchestratorList") {
    updatedTab = { ...activeTab, originSessionId: sessionId ?? activeTab.originSessionId, originProjectId: projectId };
  }
  // Terminal tabs intentionally have no branch here — see the doc comment
  // above. Do NOT add one without also taking `TerminalPanel`'s history
  // keying and its project-scope dropdown into account.

  if (!updatedTab || updatedTab === activeTab) {
    return workspace;
  }

  const updatedTabs = pane.tabs.map((tab) => (tab.id === activeTab.id ? updatedTab! : tab));
  const updatedPane = syncPaneState({ ...pane, tabs: updatedTabs });
  const updatedPanes = workspace.panes.slice();
  updatedPanes[paneIndex] = updatedPane;
  return { ...workspace, panes: updatedPanes };
}

/**
 * Find the nearest pane containing a session tab relative to the given pane.
 * Prefers left neighbors. Used for reverse-syncing: when a control surface is
 * selected, find the closest session to derive project context from.
 */
export function findNearestSessionPaneId(
  workspace: WorkspaceState,
  paneId: string,
): string | null {
  const paneLookup = new Map(workspace.panes.map((pane) => [pane.id, pane]));
  const order = flattenPaneOrder(workspace.root, paneLookup);
  const myIndex = order.indexOf(paneId);
  if (myIndex === -1) {
    return null;
  }

  for (let distance = 1; distance < order.length; distance++) {
    const leftIndex = myIndex - distance;
    if (leftIndex >= 0) {
      const leftPane = paneLookup.get(order[leftIndex]!);
      if (leftPane && getActiveTab(leftPane)?.kind === "session") {
        return leftPane.id;
      }
    }
    const rightIndex = myIndex + distance;
    if (rightIndex < order.length) {
      const rightPane = paneLookup.get(order[rightIndex]!);
      if (rightPane && getActiveTab(rightPane)?.kind === "session") {
        return rightPane.id;
      }
    }
  }

  return null;
}

function paneContainsControlPanel(pane: WorkspacePane) {
  return pane.tabs.some((tab) => tab.kind === "controlPanel");
}

/**
 * Tabs that represent "ambient" control surfaces — views that should
 * auto-follow the user's current session/project context when the pane
 * is activated. `rescopeControlSurfacePane` uses this set to decide
 * which panes are candidates for auto-rescoping.
 *
 * `"terminal"` is deliberately NOT in this set. Terminal tabs render
 * inside a `.control-panel-section-stack terminal-section-stack` wrapper
 * and expose a project-scope dropdown, so they can look like control
 * surfaces, but their `originProjectId` is pinned at creation time and
 * must not drift: the `TerminalPanel` project-scope prop flows into
 * `runTerminalCommand`'s remote-scope lookup, and auto-rescoping would
 * make command history bleed across projects. See the doc comment on
 * `rescopeControlSurfacePane` above for the full rationale.
 */
export const CONTROL_SURFACE_KINDS: ReadonlySet<string> = new Set([
  "controlPanel", "sessionList", "projectList", "orchestratorList",
  "gitStatus", "filesystem",
]);

function paneIsControlSurface(pane: WorkspacePane) {
  const activeTab = getActiveTab(pane);
  return activeTab ? CONTROL_SURFACE_KINDS.has(activeTab.kind) : false;
}

function findNearestPaneIdMatching(
  workspace: WorkspaceState,
  paneId: string,
  predicate: (pane: WorkspacePane) => boolean,
) {
  const paneLookup = new Map(workspace.panes.map((pane) => [pane.id, pane]));
  const order = flattenPaneOrder(workspace.root, paneLookup);
  const myIndex = order.indexOf(paneId);
  if (myIndex === -1) {
    return null;
  }

  for (let distance = 1; distance < order.length; distance++) {
    const leftIndex = myIndex - distance;
    if (leftIndex >= 0) {
      const leftPane = paneLookup.get(order[leftIndex]!);
      if (leftPane && predicate(leftPane)) {
        return leftPane.id;
      }
    }

    const rightIndex = myIndex + distance;
    if (rightIndex < order.length) {
      const rightPane = paneLookup.get(order[rightIndex]!);
      if (rightPane && predicate(rightPane)) {
        return rightPane.id;
      }
    }
  }

  return null;
}

function findDirectionalContentPaneId(workspace: WorkspaceState, paneId: string) {
  const paneLookup = new Map(workspace.panes.map((pane) => [pane.id, pane]));
  const order = flattenPaneOrder(workspace.root, paneLookup);
  const myIndex = order.indexOf(paneId);
  if (myIndex === -1) {
    return null;
  }

  for (let index = myIndex + 1; index < order.length; index++) {
    const candidate = paneLookup.get(order[index]!);
    if (candidate && !paneIsControlSurface(candidate)) {
      return candidate.id;
    }
  }

  for (let index = myIndex - 1; index >= 0; index--) {
    const candidate = paneLookup.get(order[index]!);
    if (candidate && !paneIsControlSurface(candidate)) {
      return candidate.id;
    }
  }

  return null;
}

function flattenPaneOrder(root: WorkspaceNode | null, paneLookup: Map<string, WorkspacePane>): string[] {
  if (!root) {
    return [];
  }
  if (root.type === "pane") {
    return paneLookup.has(root.paneId) ? [root.paneId] : [];
  }
  return [
    ...flattenPaneOrder(root.first, paneLookup),
    ...flattenPaneOrder(root.second, paneLookup),
  ];
}

/**
 * Find the nearest pane containing a control-surface view (control panel, git, files,
 * sessions, projects, orchestrators) relative to the given pane. Prefers left neighbors.
 */
export function findNearestControlSurfacePaneId(
  workspace: WorkspaceState,
  paneId: string,
): string | null {
  const paneLookup = new Map(workspace.panes.map((pane) => [pane.id, pane]));
  const order = flattenPaneOrder(workspace.root, paneLookup);
  const myIndex = order.indexOf(paneId);
  if (myIndex === -1) {
    return null;
  }

  // Search outward from the pane, preferring left.
  for (let distance = 1; distance < order.length; distance++) {
    const leftIndex = myIndex - distance;
    if (leftIndex >= 0) {
      const leftPane = paneLookup.get(order[leftIndex]!);
      if (leftPane && paneIsControlSurface(leftPane)) {
        return leftPane.id;
      }
    }
    const rightIndex = myIndex + distance;
    if (rightIndex < order.length) {
      const rightPane = paneLookup.get(order[rightIndex]!);
      if (rightPane && paneIsControlSurface(rightPane)) {
        return rightPane.id;
      }
    }
  }

  return null;
}

function isAllowedControlPanelPlacement(
  targetPane: WorkspacePane,
  tab: WorkspaceTab,
  placement: TabDropPlacement,
) {
  if (tab.kind === "controlPanel" || paneContainsControlPanel(targetPane)) {
    return placement === "left" || placement === "right";
  }

  return true;
}

function findDiffPreviewTab(
  workspace: WorkspaceState,
  changeSetId: string | null,
  diffMessageId: string,
  originSessionId: string | null,
  originProjectId: string | null,
) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceDiffPreviewTab =>
        candidate.kind === "diffPreview" &&
        (changeSetId
          ? (candidate.changeSetId ?? null) === changeSetId ||
            candidate.diffMessageId === diffMessageId
          : candidate.diffMessageId === diffMessageId) &&
        candidate.originSessionId === originSessionId &&
        (candidate.originProjectId ?? null) === originProjectId,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findRelatedDiffPreviewPaneId(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  contextSessionId: string | null,
) {
  const findMatchingPaneId = (predicate: (pane: WorkspacePane) => boolean) => {
    if (preferredPaneId) {
      return findNearestPaneIdMatching(workspace, preferredPaneId, (pane) =>
        pane.id !== preferredPaneId && predicate(pane),
      );
    }

    return workspace.panes.find(predicate)?.id ?? null;
  };

  const activeDiffPaneId = findMatchingPaneId((pane) => {
    if (getActiveTab(pane)?.kind !== "diffPreview") {
      return false;
    }

    return contextSessionId === null || pane.activeSessionId === contextSessionId;
  });
  if (activeDiffPaneId) {
    return activeDiffPaneId;
  }

  if (contextSessionId !== null) {
    const relatedDiffPaneId = findMatchingPaneId(
      (pane) =>
        pane.activeSessionId === contextSessionId &&
        pane.tabs.some((tab) => tab.kind === "diffPreview"),
    );
    if (relatedDiffPaneId) {
      return relatedDiffPaneId;
    }
  }

  return findMatchingPaneId((pane) => getActiveTab(pane)?.kind === "diffPreview");
}

function findRelatedViewerPaneId(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  contextSessionId: string | null,
) {
  const findMatchingPaneId = (predicate: (pane: WorkspacePane) => boolean) => {
    if (preferredPaneId) {
      return findNearestPaneIdMatching(workspace, preferredPaneId, (pane) =>
        pane.id !== preferredPaneId && predicate(pane),
      );
    }

    return workspace.panes.find(predicate)?.id ?? null;
  };

  const activeViewerPaneId = findMatchingPaneId((pane) => {
    const activeTabKind = getActiveTab(pane)?.kind;
    if (activeTabKind !== "source" && activeTabKind !== "diffPreview") {
      return false;
    }

    return contextSessionId === null || pane.activeSessionId === contextSessionId;
  });
  if (activeViewerPaneId) {
    return activeViewerPaneId;
  }

  if (contextSessionId !== null) {
    const relatedViewerPaneId = findMatchingPaneId(
      (pane) =>
        pane.activeSessionId === contextSessionId &&
        pane.tabs.some((tab) => tab.kind === "source" || tab.kind === "diffPreview"),
    );
    if (relatedViewerPaneId) {
      return relatedViewerPaneId;
    }
  }

  return findMatchingPaneId((pane) => {
    const activeTabKind = getActiveTab(pane)?.kind;
    return activeTabKind === "source" || activeTabKind === "diffPreview";
  });
}

function cloneWorkspaceTab(tab: WorkspaceTab): WorkspaceTab {
  return {
    ...tab,
    id: crypto.randomUUID(),
  };
}

function updateCanvasTab(
  workspace: WorkspaceState,
  canvasTabId: string,
  update: (tab: WorkspaceCanvasTab) => WorkspaceCanvasTab,
): WorkspaceState {
  let hasChanged = false;
  const panes = workspace.panes.map((pane) => {
    const canvasTabIndex = pane.tabs.findIndex(
      (tab) => tab.id === canvasTabId && tab.kind === "canvas",
    );
    if (canvasTabIndex < 0) {
      return pane;
    }

    const canvasTab = pane.tabs[canvasTabIndex];
    if (canvasTab.kind !== "canvas") {
      return pane;
    }

    const nextTab = update(canvasTab);
    if (nextTab === canvasTab) {
      return pane;
    }

    hasChanged = true;
    const nextTabs = [...pane.tabs];
    nextTabs[canvasTabIndex] = nextTab;
    return syncPaneState({
      ...pane,
      tabs: nextTabs,
    });
  });

  return hasChanged ? { ...workspace, panes } : workspace;
}

function insertTabAtIndex(tabs: WorkspaceTab[], tab: WorkspaceTab, tabIndex: number): WorkspaceTab[] {
  const nextTabs = tabs.filter((candidate) => candidate.id !== tab.id);
  const nextTabIndex = clampIndex(tabIndex, 0, nextTabs.length);
  nextTabs.splice(nextTabIndex, 0, tab);
  return nextTabs;
}

const MAX_AUTO_SPLIT_PANES = 3;

function shouldOpenTabInAdjacentPane(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  tabKind: WorkspaceTab["kind"],
) {
  if (!preferredPaneId) {
    return false;
  }

  const preferredPane = workspace.panes.find((pane) => pane.id === preferredPaneId);
  if (!preferredPane) {
    return false;
  }

  const activeTab = getActiveTab(preferredPane);
  if (!activeTab || activeTab.kind === tabKind) {
    return false;
  }

  // Don't auto-split if there are already enough content panes.
  // Only count panes that hold workspace content (sessions, source, diffs, etc.),
  // not standalone control-surface tabs (controlPanel, sessionList, projectList,
  // orchestratorList) which are lightweight panel views.
  const contentPanes = workspace.panes.filter((pane) => {
    const paneActiveTab = getActiveTab(pane);
    if (!paneActiveTab) {
      return true;
    }
    return paneActiveTab.kind !== "controlPanel"
      && paneActiveTab.kind !== "sessionList"
      && paneActiveTab.kind !== "projectList"
      && paneActiveTab.kind !== "orchestratorList";
  });
  if (contentPanes.length >= MAX_AUTO_SPLIT_PANES) {
    return false;
  }

  return true;
}

function findContextualTargetPaneId(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
  tabKind: WorkspaceTab["kind"],
) {
  const preferredPane = preferredPaneId
    ? workspace.panes.find((pane) => pane.id === preferredPaneId) ?? null
    : null;
  const preferredActiveTab = preferredPane ? getActiveTab(preferredPane) : null;

  if (preferredActiveTab?.kind === tabKind) {
    return preferredPane!.id;
  }

  if (tabKind === "source") {
    if (preferredActiveTab?.kind === "session") {
      return preferredPane!.id;
    }

    const originSessionPaneId = originSessionId ? findSessionTab(workspace, originSessionId)?.paneId ?? null : null;
    if (originSessionPaneId) {
      return originSessionPaneId;
    }

    if (preferredPane && paneContainsControlPanel(preferredPane)) {
      const nonControlPanelPaneId = findNonControlPanelPaneId(workspace, preferredPane.id);
      if (nonControlPanelPaneId && shouldOpenTabInAdjacentPane(workspace, nonControlPanelPaneId, tabKind)) {
        // Room to split - return null so the caller splits off the content pane.
        // The caller will use the content pane (not the control panel) as the split anchor.
        return null;
      }
      if (nonControlPanelPaneId) {
        return nonControlPanelPaneId;
      }
    }
  }

  if (tabKind === "sessionList" || tabKind === "orchestratorList") {
    if (preferredActiveTab?.kind === "session") {
      return preferredPane!.id;
    }

    const originSessionPaneId = originSessionId ? findSessionTab(workspace, originSessionId)?.paneId ?? null : null;
    if (originSessionPaneId) {
      return originSessionPaneId;
    }

    if (preferredPane && paneContainsControlPanel(preferredPane)) {
      const nonControlPanelPaneId = findNonControlPanelPaneId(workspace, preferredPane.id);
      if (
        nonControlPanelPaneId &&
        shouldOpenTabInAdjacentPane(workspace, nonControlPanelPaneId, tabKind)
      ) {
        return null;
      }
      if (nonControlPanelPaneId) {
        return nonControlPanelPaneId;
      }
    }
  }

  if (tabKind === "diffPreview") {
    const contextSessionId = originSessionId ?? preferredPane?.activeSessionId ?? null;
    const diffPaneId = findRelatedDiffPreviewPaneId(workspace, preferredPaneId, contextSessionId);
    if (diffPaneId) {
      return diffPaneId;
    }

    const viewerPaneId = findRelatedViewerPaneId(workspace, preferredPaneId, contextSessionId);
    if (viewerPaneId) {
      return viewerPaneId;
    }

    if (preferredPane && paneContainsControlPanel(preferredPane)) {
      const nonControlPanelPaneId = findNonControlPanelPaneId(workspace, preferredPane.id);
      if (nonControlPanelPaneId && shouldOpenTabInAdjacentPane(workspace, nonControlPanelPaneId, tabKind)) {
        return null;
      }
      if (nonControlPanelPaneId) {
        return nonControlPanelPaneId;
      }
    }
  }

  if (originSessionId) {
    const relatedPane = workspace.panes.find(
      (pane) =>
        pane.id !== preferredPaneId &&
        pane.activeSessionId === originSessionId &&
        pane.tabs.some((tab) => tab.kind === tabKind),
    );
    if (relatedPane) {
      return relatedPane.id;
    }
  }

  return null;
}
function getActiveTab(pane: WorkspacePane) {
  return pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? pane.tabs[0] ?? null;
}

const EMPTY_WORKSPACE_SOURCE_FOCUS: WorkspaceSourceFocus = {
  line: null,
  column: null,
  token: null,
};

function createOpenSourceFocus(options?: OpenSourceTabOptions | null) {
  const line = normalizeWorkspaceLineNumber(options?.line);
  if (!line) {
    return EMPTY_WORKSPACE_SOURCE_FOCUS;
  }

  return normalizeWorkspaceSourceFocus({
    line,
    column: options?.column ?? null,
    token: crypto.randomUUID(),
  });
}

function normalizeWorkspaceSourceFocus(
  focus: Partial<WorkspaceSourceFocus> | null | undefined,
): WorkspaceSourceFocus {
  const line = normalizeWorkspaceLineNumber(focus?.line);
  if (!line) {
    return EMPTY_WORKSPACE_SOURCE_FOCUS;
  }

  const token = typeof focus?.token === "string" ? focus.token.trim() : "";
  return {
    line,
    column: normalizeWorkspaceLineNumber(focus?.column),
    token: token || null,
  };
}

function sourceFocusProps(focus: WorkspaceSourceFocus) {
  if (!focus.line) {
    return {};
  }

  return {
    focusLineNumber: focus.line,
    ...(focus.column ? { focusColumnNumber: focus.column } : {}),
    ...(focus.token ? { focusToken: focus.token } : {}),
  };
}

function setSourceTabFocus(
  workspace: WorkspaceState,
  sourceTabId: string,
  focus: WorkspaceSourceFocus,
): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (!pane.tabs.some((tab) => tab.id === sourceTabId)) {
        return pane;
      }

      return syncPaneState({
        ...pane,
        tabs: pane.tabs.map((tab) => {
          if (tab.id !== sourceTabId || tab.kind !== "source") {
            return tab;
          }

          const {
            focusLineNumber: _ignoredFocusLineNumber,
            focusColumnNumber: _ignoredFocusColumnNumber,
            focusToken: _ignoredFocusToken,
            ...tabWithoutFocus
          } = tab;
          return {
            ...tabWithoutFocus,
            ...sourceFocusProps(focus),
          };
        }),
      });
    }),
  };
}

function normalizeWorkspaceLineNumber(value: number | null | undefined) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return null;
  }

  const normalized = Math.trunc(value);
  return normalized >= 1 ? normalized : null;
}

const WINDOWS_UNC_VERBATIM_PREFIX = "\\\\?\\UNC\\";
const WINDOWS_VERBATIM_PREFIX = "\\\\?\\";

function normalizeWorkspacePath(path: string | null | undefined) {
  const trimmed = path?.trim();
  if (!trimmed) {
    return null;
  }
  if (trimmed.startsWith(WINDOWS_UNC_VERBATIM_PREFIX)) {
    return `\\\\${trimmed.slice(WINDOWS_UNC_VERBATIM_PREFIX.length)}`;
  }
  if (trimmed.startsWith(WINDOWS_VERBATIM_PREFIX)) {
    return trimmed.slice(WINDOWS_VERBATIM_PREFIX.length);
  }
  return trimmed;
}

function normalizeWorkspaceIdentifier(value: string | null | undefined) {
  if (typeof value !== "string") {
    return null;
  }

  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

function normalizeWorkspaceText(value: string | null | undefined) {
  if (typeof value !== "string") {
    return null;
  }

  return value.trim() ? value : null;
}

function projectOriginProps(originProjectId: string | null) {
  return originProjectId ? { originProjectId } : {};
}

function canvasZoomProps(zoom: number) {
  return zoom === WORKSPACE_CANVAS_DEFAULT_ZOOM ? {} : { zoom };
}

export function normalizeWorkspaceCanvasZoom(value: number | null | undefined) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return WORKSPACE_CANVAS_DEFAULT_ZOOM;
  }

  const clamped = Math.min(Math.max(value, WORKSPACE_CANVAS_MIN_ZOOM), WORKSPACE_CANVAS_MAX_ZOOM);
  return Math.round(clamped * 1000) / 1000;
}

function normalizeWorkspaceCanvasCards(cards: readonly WorkspaceCanvasCard[]) {
  const seenSessionIds = new Set<string>();
  const normalizedCards: WorkspaceCanvasCard[] = [];

  for (const card of cards) {
    const normalizedCard = normalizeWorkspaceCanvasCard(card);
    if (!normalizedCard || seenSessionIds.has(normalizedCard.sessionId)) {
      continue;
    }

    seenSessionIds.add(normalizedCard.sessionId);
    normalizedCards.push(normalizedCard);
  }

  return normalizedCards;
}

function normalizeWorkspaceCanvasCard(card: WorkspaceCanvasCard | null | undefined) {
  const sessionId = normalizeWorkspaceIdentifier(card?.sessionId);
  if (!sessionId) {
    return null;
  }

  return {
    sessionId,
    x: normalizeWorkspaceCanvasCoordinate(card?.x),
    y: normalizeWorkspaceCanvasCoordinate(card?.y),
  };
}

function normalizeWorkspaceCanvasCoordinate(value: number | null | undefined) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return 0;
  }

  return Math.round(value);
}

function resolveOriginSessionId(
  originSessionId: string | null,
  activeSessionId: string | null,
  tabs: WorkspaceTab[],
) {
  return originSessionId ?? activeSessionId ?? firstSessionTabId(tabs);
}

function isSessionPaneViewMode(viewMode: PaneViewMode): viewMode is SessionPaneViewMode {
  return (
    viewMode === "session" ||
    viewMode === "prompt" ||
    viewMode === "commands" ||
    viewMode === "diffs"
  );
}

function clampIndex(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

function pruneWorkspaceNode(node: WorkspaceNode | null, availablePaneIds: Set<string>): WorkspaceNode | null {
  if (!node) {
    return null;
  }

  if (node.type === "pane") {
    return availablePaneIds.has(node.paneId) ? node : null;
  }

  const first = pruneWorkspaceNode(node.first, availablePaneIds);
  const second = pruneWorkspaceNode(node.second, availablePaneIds);
  if (!first && !second) {
    return null;
  }
  if (!first) {
    return second;
  }
  if (!second) {
    return first;
  }

  return {
    ...node,
    first,
    second,
  };
}

function removePaneFromTree(node: WorkspaceNode | null, paneId: string): WorkspaceNode | null {
  if (!node) {
    return null;
  }

  if (node.type === "pane") {
    return node.paneId === paneId ? null : node;
  }

  const first = removePaneFromTree(node.first, paneId);
  const second = removePaneFromTree(node.second, paneId);
  if (!first && !second) {
    return null;
  }
  if (!first) {
    return second;
  }
  if (!second) {
    return first;
  }

  return {
    ...node,
    first,
    second,
  };
}

function insertPaneAdjacent(
  node: WorkspaceNode,
  paneId: string,
  direction: "row" | "column",
  newPaneId: string,
  placeBefore: boolean,
): WorkspaceNode {
  if (node.type === "pane") {
    if (node.paneId !== paneId) {
      return node;
    }

    const insertedPane: WorkspaceNode = {
      type: "pane",
      paneId: newPaneId,
    };

    return {
      id: crypto.randomUUID(),
      type: "split",
      direction,
      ratio: DEFAULT_ADJACENT_PANE_SPLIT_RATIO,
      first: placeBefore ? insertedPane : node,
      second: placeBefore ? node : insertedPane,
    };
  }

  return {
    ...node,
    first: insertPaneAdjacent(node.first, paneId, direction, newPaneId, placeBefore),
    second: insertPaneAdjacent(node.second, paneId, direction, newPaneId, placeBefore),
  };
}

function updateSplitRatioInNode(node: WorkspaceNode, splitId: string, ratio: number): WorkspaceNode {
  if (node.type === "pane") {
    return node;
  }

  if (node.id === splitId) {
    return {
      ...node,
      ratio,
    };
  }

  return {
    ...node,
    first: updateSplitRatioInNode(node.first, splitId, ratio),
    second: updateSplitRatioInNode(node.second, splitId, ratio),
  };
}

