import type {
  GitDiffDocumentContent,
  GitDiffRequestPayload,
  GitDiffSection,
} from "./api";
import type { DiffMessage, Session } from "./types";
import {
  EMPTY_WORKSPACE_SOURCE_FOCUS,
  canvasZoomProps,
  createOpenSourceFocus,
  normalizeWorkspaceCanvasCard,
  normalizeWorkspaceCanvasCards,
  normalizeWorkspaceCanvasZoom,
  normalizeWorkspaceIdentifier,
  normalizeWorkspacePath,
  normalizeWorkspaceSourceFocus,
  projectOriginProps,
  sourceFocusProps,
} from "./workspace-normalize";
export { normalizeWorkspaceCanvasZoom } from "./workspace-normalize";
import {
  createCanvasTab,
  createControlPanelTab,
  createDiffPreviewTab,
  createFilesystemTab,
  createGitStatusTab,
  createInstructionDebuggerTab,
  createMailboxTab,
  createOrchestratorCanvasTab,
  createOrchestratorListTab,
  createProjectListTab,
  createResponseBoardTab,
  createSessionListTab,
  createSessionTab,
  createSourceTab,
  createTerminalTab,
} from "./workspace-tabs";
import {
  selectActiveTabAfterRemoval,
  withActivatedPaneTab,
  withPrunedPaneTabVisitHistory,
} from "./workspace-tab-history";
import {
  normalizeResponseBoardViews,
  setResponseBoardWorkspaceStateInPanes,
} from "./workspace-response-board";
export {
  createCanvasTab,
  createControlPanelTab,
  createDiffPreviewTab,
  createFilesystemTab,
  createGitStatusTab,
  createInstructionDebuggerTab,
  createMailboxTab,
  createOrchestratorCanvasTab,
  createOrchestratorListTab,
  createProjectListTab,
  createResponseBoardTab,
  createSessionListTab,
  createSessionTab,
  createSourceTab,
  createTerminalTab,
} from "./workspace-tabs";
export type {
  OpenSourceTabOptions,
  PaneViewMode,
  SessionPaneViewMode,
  TabDropPlacement,
  WorkspaceCanvasCard,
  WorkspaceCanvasTab,
  WorkspaceControlPanelTab,
  WorkspaceDiffPreviewTab,
  WorkspaceFilesystemTab,
  WorkspaceGitStatusTab,
  WorkspaceInstructionDebuggerTab,
  WorkspaceMailboxTab,
  WorkspaceNode,
  WorkspaceOrchestratorCanvasTab,
  WorkspaceOrchestratorListTab,
  WorkspaceOriginOnlyTab,
  WorkspacePane,
  WorkspaceProjectListTab,
  WorkspaceResponseBoardTab,
  WorkspaceResponseBoardView,
  WorkspaceSessionListTab,
  WorkspaceSessionTab,
  WorkspaceSourceFocus,
  WorkspaceSourceTab,
  WorkspaceState,
  WorkspaceTab,
  WorkspaceTerminalTab,
} from "./workspace-types";
import {
  DEFAULT_CONTROL_PANEL_DOCK_WIDTH_RATIO,
  WORKSPACE_CANVAS_DEFAULT_ZOOM,
  type OpenSourceTabOptions,
  type PaneViewMode,
  type SessionPaneViewMode,
  type TabDropPlacement,
  type WorkspaceCanvasCard,
  type WorkspaceCanvasTab,
  type WorkspaceControlPanelTab,
  type WorkspaceDiffPreviewTab,
  type WorkspaceFilesystemTab,
  type WorkspaceGitStatusTab,
  type WorkspaceInstructionDebuggerTab,
  type WorkspaceMailboxTab,
  type WorkspaceNode,
  type WorkspaceOrchestratorListTab,
  type WorkspaceOriginOnlyTab,
  type WorkspacePane,
  type WorkspaceProjectListTab,
  type WorkspaceResponseBoardTab,
  type WorkspaceResponseBoardView,
  type WorkspaceSessionListTab,
  type WorkspaceSessionTab,
  type WorkspaceSourceFocus,
  type WorkspaceSourceTab,
  type WorkspaceState,
  type WorkspaceTab,
  type WorkspaceTerminalTab,
} from "./workspace-types";
export {
  DEFAULT_CONTROL_PANEL_DOCK_WIDTH_RATIO,
  WORKSPACE_CANVAS_DEFAULT_ZOOM,
  WORKSPACE_CANVAS_MAX_ZOOM,
  WORKSPACE_CANVAS_MIN_ZOOM,
} from "./workspace-types";
const DEFAULT_ADJACENT_PANE_SPLIT_RATIO = 0.5;

export type ReconcileWorkspaceStateOptions = {
  pruneDelegatedChildSessionTabs?: boolean;
  preserveSessionIds?: readonly string[];
};

// Returns whether the workspace still points at delegated child sessions.
// `preserveSessionIds` exempts child sessions that were intentionally opened
// in the current UI session and should not be treated as stale restored tabs.
export function workspaceHasDelegatedChildSessionReferences(
  workspace: WorkspaceState,
  sessions: readonly Session[],
  preserveSessionIds: readonly string[] = [],
) {
  const preservedSessionIds = new Set(preserveSessionIds);
  const delegatedChildSessionIds = new Set(
    sessions.flatMap((session) =>
      session.parentDelegationId && !preservedSessionIds.has(session.id)
        ? [session.id]
        : [],
    ),
  );
  if (delegatedChildSessionIds.size === 0) {
    return false;
  }

  return workspace.panes.some((pane) => {
    if (
      pane.activeSessionId &&
      delegatedChildSessionIds.has(pane.activeSessionId)
    ) {
      return true;
    }

    return pane.tabs.some((tab) => {
      if (tab.kind === "session") {
        return delegatedChildSessionIds.has(tab.sessionId);
      }
      if (tab.kind === "canvas") {
        return (
          (!!tab.originSessionId &&
            delegatedChildSessionIds.has(tab.originSessionId)) ||
          tab.cards.some((card) => delegatedChildSessionIds.has(card.sessionId))
        );
      }
      return (
        "originSessionId" in tab &&
        !!tab.originSessionId &&
        delegatedChildSessionIds.has(tab.originSessionId)
      );
    });
  });
}

// Collects every session id directly referenced by the workspace tree, including
// tab origins and canvas cards, so restore-time pruning can stay scoped to ids
// that came from persisted layout state.
export function collectWorkspaceSessionReferences(workspace: WorkspaceState) {
  const sessionIds = new Set<string>();
  workspace.panes.forEach((pane) => {
    if (pane.activeSessionId) {
      sessionIds.add(pane.activeSessionId);
    }
    pane.tabs.forEach((tab) => {
      if (tab.kind === "session") {
        sessionIds.add(tab.sessionId);
      }
      if ("originSessionId" in tab && tab.originSessionId) {
        sessionIds.add(tab.originSessionId);
      }
      if (tab.kind === "canvas") {
        tab.cards.forEach((card) => {
          sessionIds.add(card.sessionId);
        });
      }
    });
  });
  return sessionIds;
}

export function normalizeWorkspaceStatePaths(
  workspace: WorkspaceState,
): WorkspaceState {
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
            const normalizedDisplayPath = normalizeWorkspacePath(
              tab.displayPath,
            );
            return {
              ...tab,
              ...(typeof tab.displayPath === "undefined"
                ? {}
                : { displayPath: normalizedDisplayPath }),
              filePath: normalizeWorkspacePath(tab.filePath),
            };
          }
          return tab;
        }),
      }),
    ),
  };
}

export function reconcileWorkspaceState(
  current: WorkspaceState,
  sessions: Session[],
  options: ReconcileWorkspaceStateOptions = {},
): WorkspaceState {
  const preservedSessionIds = new Set(options.preserveSessionIds ?? []);
  const shouldPruneDelegatedChildSessionTabs =
    options.pruneDelegatedChildSessionTabs === true;
  const availableSessions = sessions.filter((session) => {
    if (
      shouldPruneDelegatedChildSessionTabs &&
      session.parentDelegationId &&
      !preservedSessionIds.has(session.id)
    ) {
      return false;
    }
    return true;
  });
  const availableSessionIds = new Set(
    availableSessions.map((session) => session.id),
  );
  let panes = current.panes.map((pane) => {
    const tabs = pane.tabs.flatMap((tab): WorkspaceTab[] => {
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
        const {
          originProjectId: _ignoredOriginProjectId,
          ...tabWithoutOriginProjectId
        } = tab;
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
        const {
          originProjectId: _ignoredOriginProjectId,
          ...tabWithoutOriginProjectId
        } = tab;
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
        const {
          originProjectId: _ignoredOriginProjectId,
          ...tabWithoutOriginProjectId
        } = tab;
        return [
          {
            ...tabWithoutOriginProjectId,
            originSessionId,
            ...projectOriginProps(originProjectId),
            workdir: normalizeWorkspacePath(tab.workdir),
          },
        ];
      }

      if (tab.kind === "mailbox") {
        const mailboxId = normalizeWorkspaceIdentifier(tab.mailboxId);
        if (!mailboxId || !originSessionId) {
          return [];
        }
        const {
          originProjectId: _ignoredOriginProjectId,
          ...tabWithoutOriginProjectId
        } = tab;
        return [
          {
            ...tabWithoutOriginProjectId,
            mailboxId,
            originSessionId,
            ...projectOriginProps(originProjectId),
          },
        ];
      }

      if (tab.kind === "responseBoard") {
        const {
          originProjectId: _ignoredOriginProjectId,
          activeBoardTabId: _ignoredActiveBoardTabId,
          boardViews: _ignoredBoardViews,
          ...tabWithoutOriginProjectId
        } = tab;
        return [
          {
            ...tabWithoutOriginProjectId,
            originSessionId,
            ...projectOriginProps(originProjectId),
            activeBoardTabId: normalizeWorkspaceIdentifier(
              tab.activeBoardTabId,
            ),
            boardViews: normalizeResponseBoardViews(tab.boardViews),
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
        const {
          originProjectId: _ignoredOriginProjectId,
          zoom: _ignoredZoom,
          ...tabWithoutOriginProjectId
        } = tab;
        return [
          {
            ...tabWithoutOriginProjectId,
            cards: normalizeWorkspaceCanvasCards(tab.cards).filter((card) =>
              availableSessionIds.has(card.sessionId),
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
        const normalizedTemplateId = normalizeWorkspaceIdentifier(
          tab.templateId,
        );
        return [
          {
            ...tabWithoutSpecialFields,
            originSessionId,
            ...projectOriginProps(originProjectId),
            ...(normalizedTemplateId
              ? { templateId: normalizedTemplateId }
              : {}),
            ...(tab.startMode === "new" ? { startMode: "new" as const } : {}),
          },
        ];
      }

      if (tab.kind === "instructionDebugger") {
        const {
          originProjectId: _ignoredOriginProjectId,
          ...tabWithoutOriginProjectId
        } = tab;
        return [
          {
            ...tabWithoutOriginProjectId,
            originSessionId,
            ...projectOriginProps(originProjectId),
            workdir: normalizeWorkspacePath(tab.workdir),
          },
        ];
      }

      const {
        originProjectId: _ignoredOriginProjectId,
        ...tabWithoutOriginProjectId
      } = tab;
      const normalizedChangeSetId = normalizeWorkspaceIdentifier(
        tab.changeSetId,
      );
      const normalizedDisplayPath = normalizeWorkspacePath(tab.displayPath);
      return [
        {
          ...tabWithoutOriginProjectId,
          originSessionId,
          ...projectOriginProps(originProjectId),
          ...(normalizedChangeSetId
            ? { changeSetId: normalizedChangeSetId }
            : {}),
          ...(typeof tab.displayPath === "undefined"
            ? {}
            : { displayPath: normalizedDisplayPath }),
          filePath: normalizeWorkspacePath(tab.filePath),
        },
      ];
    });
    const activeTabId = tabs.some((tab) => tab.id === pane.activeTabId)
      ? pane.activeTabId
      : selectActiveTabAfterRemoval(
          pane,
          tabs,
          pane.activeTabId,
          tabs[0]?.id ?? null,
        );
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
  if (shouldPruneDelegatedChildSessionTabs) {
    const originallyEmptyPaneIds = new Set(
      current.panes
        .filter((pane) => pane.tabs.length === 0)
        .map((pane) => pane.id),
    );
    panes = panes.filter(
      (pane) => pane.tabs.length > 0 || originallyEmptyPaneIds.has(pane.id),
    );
  }

  let root = pruneWorkspaceNode(
    current.root,
    new Set(panes.map((pane) => pane.id)),
  );

  if (!root && panes.length > 0) {
    root = {
      type: "pane",
      paneId: panes[0].id,
    };
  }

  if (!root && availableSessions.length > 0) {
    const initialPane = createPane(createSessionTab(availableSessions[0].id));
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

  return withActivePaneRoutingState(current, panes, activePaneId, { root });
}

export function findWorkspacePaneIdForSession(
  workspace: WorkspaceState,
  sessionId: string,
) {
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
    // Opening is navigation, not layout mutation. Existing tabs keep the pane
    // chosen by the user; explicit drag/drop remains the only move operation.
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

// Refused placements must return `workspace` by identity. The drag/drop
// transaction uses reference equality to distinguish refusal from commit.
export function placeSessionDropInWorkspaceState(
  workspace: WorkspaceState,
  sessionId: string,
  targetPaneId: string,
  placement: TabDropPlacement,
  tabIndex?: number,
  newSessionTabId?: string,
): WorkspaceState {
  if (placement === "tabs") {
    const existing = findSessionTab(workspace, sessionId);
    const targetPane = workspace.panes.find(
      (pane) => pane.id === targetPaneId,
    );
    const sessionTab =
      existing?.tab ?? createSessionTab(sessionId, newSessionTabId);
    if (
      !targetPane ||
      !isAllowedControlPanelPlacement(targetPane, sessionTab, placement)
    ) {
      return workspace;
    }

    if (existing) {
      return activatePane(
        moveWorkspaceTabToPane(
          workspace,
          existing.paneId,
          existing.tab.id,
          targetPaneId,
          tabIndex,
        ),
        targetPaneId,
        existing.tab.id,
      );
    }

    return addWorkspaceTabToPane(
      workspace,
      targetPaneId,
      sessionTab,
      tabIndex,
    );
  }

  if (findWorkspacePaneIdForSession(workspace, sessionId)) {
    return openSessionInWorkspaceState(workspace, sessionId, targetPaneId);
  }

  // Keep the gesture-owned id on both sides of the clone boundary. Passing it
  // only to createSessionTab would let placeExternalTab mint a different id,
  // breaking the caller's unambiguous structural commit evidence.
  const sessionTab = createSessionTab(sessionId, newSessionTabId);
  return placeExternalTab(
    workspace,
    sessionTab,
    targetPaneId,
    placement,
    tabIndex,
    sessionTab.id,
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
    typeof originProjectIdOrOptions === "string" ||
    originProjectIdOrOptions === null
      ? originProjectIdOrOptions
      : null;
  const resolvedOptions =
    typeof originProjectIdOrOptions === "string" ||
    originProjectIdOrOptions === null
      ? options
      : originProjectIdOrOptions;
  const normalizedPath = normalizeWorkspacePath(path);
  const focus = createOpenSourceFocus(resolvedOptions);
  const nextTab = createSourceTab(
    normalizedPath,
    originSessionId,
    originProjectId,
    focus,
  );
  const viewerTarget = resolveViewerOpenTarget(
    workspace,
    preferredPaneId,
    originSessionId,
    resolvedOptions?.allowViewerSplit !== false,
  );
  const viewerWorkspace = workspaceWithRememberedViewerTarget(
    workspace,
    viewerTarget,
  );
  if (resolvedOptions?.openInNewTab) {
    if (viewerTarget.splitAnchorPaneId) {
      return openTabInAdjacentPane(
        viewerWorkspace,
        viewerTarget.splitAnchorPaneId,
        nextTab,
        "row",
        false,
      );
    }
    if (viewerTarget.targetPaneId) {
      return openTabInWorkspaceState(
        viewerWorkspace,
        nextTab,
        viewerTarget.targetPaneId,
      );
    }
    return openContextualTabInWorkspaceState(
      viewerWorkspace,
      nextTab,
      null,
      preferredPaneId,
      originSessionId,
    );
  }

  if (normalizedPath) {
    const existing = findSourceTab(workspace, normalizedPath);
    if (existing) {
      const activatedWorkspace = activatePane(
        workspace,
        existing.paneId,
        existing.tab.id,
      );
      return setSourceTabFocus(activatedWorkspace, existing.tab.id, focus);
    }
  }

  if (viewerTarget.splitAnchorPaneId) {
    return openTabInAdjacentPane(
      viewerWorkspace,
      viewerTarget.splitAnchorPaneId,
      nextTab,
      "row",
      false,
    );
  }
  if (viewerTarget.targetPaneId) {
    return openTabInWorkspaceState(
      viewerWorkspace,
      nextTab,
      viewerTarget.targetPaneId,
    );
  }

  return openContextualTabInWorkspaceState(
    viewerWorkspace,
    nextTab,
    null,
    preferredPaneId,
    originSessionId,
  );
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
  const normalizedOriginSessionId =
    normalizeWorkspaceIdentifier(originSessionId);
  const normalizedOriginProjectId =
    normalizeWorkspaceIdentifier(originProjectId);
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
    createTerminalTab(
      normalizedWorkdir,
      normalizedOriginSessionId,
      normalizedOriginProjectId,
    ),
    preferredPaneId,
  );
}

export function openMailboxInWorkspaceState(
  workspace: WorkspaceState,
  mailboxId: string,
  preferredPaneId: string | null,
  originSessionId: string,
  originProjectId: string | null = null,
): WorkspaceState {
  const normalizedMailboxId = normalizeWorkspaceIdentifier(mailboxId);
  const normalizedOriginSessionId =
    normalizeWorkspaceIdentifier(originSessionId);
  if (!normalizedMailboxId || !normalizedOriginSessionId) {
    return workspace;
  }

  const existing = findMailboxTab(workspace, normalizedMailboxId);
  if (existing) {
    const panes = workspace.panes.map((pane) =>
      pane.id === existing.paneId
        ? syncPaneState(
            withActivatedPaneTab(
              pane,
              existing.tab.id,
              pane.tabs.map((tab) =>
                tab.id === existing.tab.id && tab.kind === "mailbox"
                  ? {
                      ...tab,
                      originSessionId: normalizedOriginSessionId,
                      ...projectOriginProps(
                        normalizeWorkspaceIdentifier(originProjectId),
                      ),
                      refreshToken: crypto.randomUUID(),
                    }
                  : tab,
              ),
            ),
          )
        : pane,
    );
    return withActivePaneRoutingState(workspace, panes, existing.paneId);
  }

  return openTabInWorkspaceState(
    workspace,
    createMailboxTab(
      normalizedMailboxId,
      normalizedOriginSessionId,
      originProjectId,
    ),
    preferredPaneId,
  );
}

export function openResponseBoardInWorkspaceState(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
  originProjectId: string | null = null,
  activeBoardTabId: string | null = null,
): WorkspaceState {
  const normalizedBoardTabId = normalizeWorkspaceIdentifier(activeBoardTabId);
  const preferredPane = preferredPaneId
    ? workspace.panes.find((pane) => pane.id === preferredPaneId)
    : null;
  const preferredExisting = preferredPane?.tabs.find(
    (tab): tab is WorkspaceResponseBoardTab => tab.kind === "responseBoard",
  );
  const existing = preferredExisting
    ? { paneId: preferredPane!.id, tab: preferredExisting }
    : !preferredPaneId
      ? findResponseBoardTab(workspace)
      : null;
  if (existing) {
    const panes = workspace.panes.map((pane) =>
      pane.id === existing.paneId
        ? syncPaneState(
            withActivatedPaneTab(
              pane,
              existing.tab.id,
              pane.tabs.map((tab) =>
                tab.id === existing.tab.id && tab.kind === "responseBoard"
                  ? {
                      ...tab,
                      originSessionId:
                        normalizeWorkspaceIdentifier(originSessionId),
                      ...projectOriginProps(
                        normalizeWorkspaceIdentifier(originProjectId),
                      ),
                      refreshToken: crypto.randomUUID(),
                      activeBoardTabId:
                        normalizedBoardTabId ?? tab.activeBoardTabId ?? null,
                    }
                  : tab,
              ),
            ),
          )
        : pane,
    );
    return withActivePaneRoutingState(workspace, panes, existing.paneId);
  }

  return openTabInWorkspaceState(
    workspace,
    createResponseBoardTab(
      originSessionId,
      originProjectId,
      normalizedBoardTabId,
    ),
    preferredPaneId,
  );
}

export function setResponseBoardWorkspaceState(
  workspace: WorkspaceState,
  workspaceTabId: string,
  activeBoardTabId: string,
  view: WorkspaceResponseBoardView,
  knownBoardTabIds?: readonly string[],
): WorkspaceState {
  return setResponseBoardWorkspaceStateInPanes(
    workspace,
    workspaceTabId,
    activeBoardTabId,
    view,
    knownBoardTabIds,
    syncPaneState,
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
            ...projectOriginProps(
              normalizeWorkspaceIdentifier(originProjectId),
            ),
          } satisfies WorkspaceCanvasTab)
        : existing.tab;
    const updatedWorkspace =
      nextTab === existing.tab
        ? workspace
        : replaceWorkspaceTabInPane(
            workspace,
            existing.paneId,
            existing.tab.id,
            nextTab,
          );

    return activatePane(updatedWorkspace, existing.paneId, existing.tab.id);
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
            ...projectOriginProps(
              normalizeWorkspaceIdentifier(originProjectId),
            ),
          } satisfies WorkspaceOrchestratorListTab)
        : existing.tab;
    const updatedWorkspace =
      nextTab === existing.tab
        ? workspace
        : replaceWorkspaceTabInPane(
            workspace,
            existing.paneId,
            existing.tab.id,
            nextTab,
          );

    return activatePane(updatedWorkspace, existing.paneId, existing.tab.id);
  }

  const nextTab = createOrchestratorListTab(originSessionId, originProjectId);
  if (targetPaneId) {
    return openTabInWorkspaceState(workspace, nextTab, targetPaneId);
  }

  return openContextualTabInWorkspaceState(
    workspace,
    nextTab,
    null,
    preferredPaneId,
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
            ...projectOriginProps(
              normalizeWorkspaceIdentifier(originProjectId),
            ),
          } satisfies WorkspaceSessionListTab)
        : existing.tab;
    const updatedWorkspace =
      nextTab === existing.tab
        ? workspace
        : replaceWorkspaceTabInPane(
            workspace,
            existing.paneId,
            existing.tab.id,
            nextTab,
          );

    return activatePane(updatedWorkspace, existing.paneId, existing.tab.id);
  }

  const nextTab = createSessionListTab(originSessionId, originProjectId);
  if (targetPaneId) {
    return openTabInWorkspaceState(workspace, nextTab, targetPaneId);
  }

  return openContextualTabInWorkspaceState(
    workspace,
    nextTab,
    null,
    preferredPaneId,
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
  const existing = findInstructionDebuggerTab(
    workspace,
    normalizedWorkdir,
    originSessionId,
  );
  if (existing) {
    return activatePane(workspace, existing.paneId, existing.tab.id);
  }

  return openTabInWorkspaceState(
    workspace,
    createInstructionDebuggerTab(
      normalizedWorkdir,
      originSessionId,
      originProjectId,
    ),
    preferredPaneId,
  );
}

// Layout normalization may add the missing control surface, but it must keep
// every existing pane id and tab-to-pane membership stable. Post-layout
// drag/drop verification keys on those identities.
export function ensureControlPanelInWorkspaceState(
  workspace: WorkspaceState,
): WorkspaceState {
  if (findControlPanelTab(workspace)) {
    return workspace;
  }

  return openTabInWorkspaceState(
    workspace,
    createControlPanelTab(null, null),
    findDefaultControlPanelAnchorPaneId(workspace),
  );
}

// Docking only restructures the pane tree. Preserve pane ids and tab-to-pane
// membership because drag/drop commits are verified after this layout pass.
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
  const nextRatio =
    side === "left" ? controlPanelWidthRatio : 1 - controlPanelWidthRatio;

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
    displayPath?: string | null;
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
    allowViewerSplit?: boolean;
  },
): WorkspaceState {
  const nextTab = createDiffPreviewTab(tab);
  const viewerTarget = resolveViewerOpenTarget(
    workspace,
    preferredPaneId,
    tab.originSessionId,
    options?.allowViewerSplit !== false,
  );
  const viewerWorkspace = workspaceWithRememberedViewerTarget(
    workspace,
    viewerTarget,
  );
  if (options?.openInNewTab) {
    if (viewerTarget.splitAnchorPaneId) {
      return openTabInAdjacentPane(
        viewerWorkspace,
        viewerTarget.splitAnchorPaneId,
        nextTab,
        "row",
        false,
      );
    }
    if (viewerTarget.targetPaneId) {
      return openTabInWorkspaceState(
        viewerWorkspace,
        nextTab,
        viewerTarget.targetPaneId,
      );
    }
    return openContextualTabInWorkspaceState(
      viewerWorkspace,
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

  if (options?.reuseActiveViewerTab && !viewerTarget.rememberTargetAsViewer) {
    if (viewerTarget.splitAnchorPaneId) {
      return openTabInAdjacentPane(
        viewerWorkspace,
        viewerTarget.splitAnchorPaneId,
        nextTab,
        "row",
        false,
      );
    }
    if (viewerTarget.targetPaneId) {
      return replaceActiveViewerTabInPane(
        viewerWorkspace,
        viewerTarget.targetPaneId,
        nextTab,
      );
    }
  }

  if (viewerTarget.splitAnchorPaneId) {
    return openTabInAdjacentPane(
      viewerWorkspace,
      viewerTarget.splitAnchorPaneId,
      nextTab,
      "row",
      false,
    );
  }
  if (viewerTarget.targetPaneId) {
    return openTabInWorkspaceState(
      viewerWorkspace,
      nextTab,
      viewerTarget.targetPaneId,
    );
  }

  return openContextualTabInWorkspaceState(
    viewerWorkspace,
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

  const panes = workspace.panes.map((pane) => {
    if (pane.id !== paneId) {
      return pane;
    }

    const activeTabId =
      tabId && pane.tabs.some((tab) => tab.id === tabId)
        ? tabId
        : (pane.activeTabId ?? pane.tabs[0]?.id ?? null);

    return syncPaneState(
      activeTabId === pane.activeTabId
        ? pane
        : withActivatedPaneTab(pane, activeTabId),
    );
  });
  return withActivePaneRoutingState(workspace, panes, paneId);
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

  const closedTabIndex = pane.tabs.findIndex(
    (candidate) => candidate.id === tabId,
  );
  const nextTabs = pane.tabs.filter((candidate) => candidate.id !== tabId);
  if (nextTabs.length === 0) {
    const panes = workspace.panes.filter(
      (candidate) => candidate.id !== paneId,
    );
    const root = removePaneFromTree(workspace.root, paneId);

    const activePaneId = panes.some(
      (candidate) => candidate.id === workspace.activePaneId,
    )
      ? workspace.activePaneId
      : (panes[0]?.id ?? null);
    return withActivePaneRoutingState(workspace, panes, activePaneId, { root });
  }

  const adjacentTabId =
    nextTabs[Math.min(Math.max(closedTabIndex, 0), nextTabs.length - 1)]?.id ??
    null;
  const nextActiveTabId = selectActiveTabAfterRemoval(
    pane,
    nextTabs,
    tabId,
    adjacentTabId,
  );

  const panes = workspace.panes.map((candidate) => {
    if (candidate.id !== paneId) {
      return candidate;
    }

    return syncPaneState(
      withActivatedPaneTab(candidate, nextActiveTabId, nextTabs),
    );
  });
  return withActivePaneRoutingState(workspace, panes, paneId);
}

export function splitPane(
  workspace: WorkspaceState,
  paneId: string,
  direction: "row" | "column",
  newPaneId: string = crypto.randomUUID(),
): WorkspaceState {
  const pane = workspace.panes.find((candidate) => candidate.id === paneId);
  if (!pane || !workspace.root) {
    return workspace;
  }

  const activeTab =
    pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? null;
  const tabToMove = pane.tabs.length > 1 ? activeTab : null;
  const newPane = createPane(
    tabToMove,
    pane.lastSessionViewMode,
    newPaneId,
  );
  const panes = workspace.panes.map((candidate) => {
    if (candidate.id !== paneId || !tabToMove) {
      return candidate;
    }

    const remainingTabs = candidate.tabs.filter(
      (tab) => tab.id !== tabToMove.id,
    );
    const nextActiveTabId = selectActiveTabAfterRemoval(
      candidate,
      remainingTabs,
      tabToMove.id,
      remainingTabs[0]?.id ?? null,
    );

    return syncPaneState(
      withActivatedPaneTab(candidate, nextActiveTabId, remainingTabs),
    );
  });

  return withActivePaneRoutingState(
    workspace,
    [...panes, newPane],
    newPane.id,
    {
      root: insertPaneAdjacent(
        workspace.root,
        paneId,
        direction,
        newPane.id,
        false,
      ),
    },
  );
}

// Refused placements must return `workspace` by identity. The drag/drop
// transaction uses reference equality to distinguish refusal from commit.
export function placeDraggedTab(
  workspace: WorkspaceState,
  sourcePaneId: string,
  tabId: string,
  targetPaneId: string,
  placement: TabDropPlacement,
  tabIndex?: number,
  newPaneId: string = crypto.randomUUID(),
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
      const sourceTabIndex = sourcePane.tabs.findIndex(
        (tab) => tab.id === tabId,
      );
      const adjustedTabIndex =
        sourceTabIndex >= 0 && requestedTabIndex > sourceTabIndex
          ? requestedTabIndex - 1
          : requestedTabIndex;

      return addWorkspaceTabToPane(
        workspace,
        targetPaneId,
        draggedTab,
        adjustedTabIndex,
      );
    }

    const withoutSource = closeWorkspaceTab(workspace, sourcePaneId, tabId);
    return addWorkspaceTabToPane(
      withoutSource,
      targetPaneId,
      draggedTab,
      requestedTabIndex,
    );
  }

  if (sourcePaneId === targetPaneId && sourcePane.tabs.length <= 1) {
    return workspace;
  }

  const withoutSource = closeWorkspaceTab(workspace, sourcePaneId, tabId);
  if (
    !withoutSource.root ||
    !withoutSource.panes.some((pane) => pane.id === targetPaneId)
  ) {
    return workspace;
  }

  const newPane = createPane(
    draggedTab,
    targetPane.lastSessionViewMode,
    newPaneId,
  );
  const direction =
    placement === "left" || placement === "right" ? "row" : "column";
  const placeBefore = placement === "left" || placement === "top";

  return withActivePaneRoutingState(
    withoutSource,
    [...withoutSource.panes, newPane],
    newPane.id,
    {
      root: insertPaneAdjacent(
        withoutSource.root,
        targetPaneId,
        direction,
        newPane.id,
        placeBefore,
      ),
    },
  );
}

// Refused placements must return `workspace` by identity. The drag/drop
// transaction uses reference equality to distinguish refusal from commit.
export function placeExternalTab(
  workspace: WorkspaceState,
  tab: WorkspaceTab,
  targetPaneId: string,
  placement: TabDropPlacement,
  tabIndex?: number,
  transferredTabId?: string,
): WorkspaceState {
  const transferredTab = cloneWorkspaceTab(tab, transferredTabId);
  const targetPane = workspace.panes.find((pane) => pane.id === targetPaneId);
  if (!targetPane || !workspace.root) {
    // A drag/drop gesture names a concrete destination. If that destination
    // vanished before the reducer ran, refuse the transfer instead of routing
    // the cloned tab somewhere the user did not choose.
    return workspace;
  }

  if (placement === "tabs") {
    if (
      !isAllowedControlPanelPlacement(targetPane, transferredTab, placement)
    ) {
      return workspace;
    }

    // An explicit tab-rail drop names its destination exactly. It must not be
    // redirected by the automatic opener policy.
    return addWorkspaceTabToPane(
      workspace,
      targetPaneId,
      transferredTab,
      tabIndex,
    );
  }

  if (!isAllowedControlPanelPlacement(targetPane, transferredTab, placement)) {
    return workspace;
  }

  const newPane = createPane(transferredTab, targetPane.lastSessionViewMode);
  const direction =
    placement === "left" || placement === "right" ? "row" : "column";
  const placeBefore = placement === "left" || placement === "top";

  return withActivePaneRoutingState(
    workspace,
    [...workspace.panes, newPane],
    newPane.id,
    {
      root: insertPaneAdjacent(
        workspace.root,
        targetPaneId,
        direction,
        newPane.id,
        placeBefore,
      ),
    },
  );
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
  paneId: string = crypto.randomUUID(),
): WorkspacePane {
  return syncPaneState({
    id: paneId,
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
    const cards = tab.cards.filter(
      (card) => card.sessionId !== normalizedSessionId,
    );
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
      (tab): tab is WorkspaceSourceTab =>
        tab.id === currentPane.activeTabId && tab.kind === "source",
    )?.id ?? null;
  const existing = nextPath ? findSourceTab(workspace, nextPath) : null;
  if (existing && existing.tab.id !== activeSourceTabId) {
    return activatePane(
      setSourceTabFocus(
        workspace,
        existing.tab.id,
        EMPTY_WORKSPACE_SOURCE_FOCUS,
      ),
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
            originSessionId:
              activeTab.originSessionId ?? pane.activeSessionId ?? null,
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
  const panes = workspace.panes.map((pane) => {
    if (pane.id !== paneId) {
      return pane;
    }

    return syncPaneState(
      withActivatedPaneTab(
        pane,
        tab.id,
        insertTabAtIndex(pane.tabs, tab, tabIndex ?? pane.tabs.length),
      ),
    );
  });
  return withActivePaneRoutingState(workspace, panes, paneId);
}

function replaceActiveViewerTabInPane(
  workspace: WorkspaceState,
  paneId: string,
  tab: WorkspaceTab,
): WorkspaceState {
  const pane =
    workspace.panes.find((candidate) => candidate.id === paneId) ?? null;
  const activeTab = pane ? getActiveTab(pane) : null;
  if (
    !pane ||
    !activeTab ||
    (activeTab.kind !== "source" && activeTab.kind !== "diffPreview")
  ) {
    return addWorkspaceTabToPane(workspace, paneId, tab);
  }

  const panes = workspace.panes.map((candidate) => {
    if (candidate.id !== paneId) {
      return candidate;
    }

    return syncPaneState(
      withActivatedPaneTab(
        candidate,
        tab.id,
        candidate.tabs.map((entry) =>
          entry.id === activeTab.id ? tab : entry,
        ),
      ),
    );
  });
  return withActivePaneRoutingState(workspace, panes, paneId);
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

    const sourceTabIndex = sourcePane.tabs.findIndex(
      (candidate) => candidate.id === tabId,
    );
    const adjustedTabIndex =
      sourceTabIndex >= 0 && tabIndex > sourceTabIndex
        ? tabIndex - 1
        : tabIndex;
    return addWorkspaceTabToPane(
      workspace,
      sourcePaneId,
      tab,
      adjustedTabIndex,
    );
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

export function stripLoadingGitDiffPreviewTabsFromWorkspaceState(
  workspace: WorkspaceState,
): WorkspaceState {
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
    nextWorkspace = closeWorkspaceTab(
      nextWorkspace,
      loadingTab.paneId,
      loadingTab.tabId,
    );
  }

  return nextWorkspace;
}

export function stripDiffPreviewDocumentContentFromWorkspaceState(
  workspace: WorkspaceState,
): WorkspaceState {
  let changed = false;
  const panes = workspace.panes.map((pane) => {
    let paneChanged = false;
    const tabs = pane.tabs.map((tab) => {
      if (tab.kind !== "diffPreview" || !tab.documentContent) {
        return tab;
      }

      const {
        documentContent: _documentContent,
        ...tabWithoutDocumentContent
      } = tab;
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

export function getSplitRatio(
  node: WorkspaceNode | null,
  splitId: string,
): number | null {
  if (!node || node.type === "pane") {
    return null;
  }

  if (node.id === splitId) {
    return node.ratio;
  }

  return (
    getSplitRatio(node.first, splitId) ?? getSplitRatio(node.second, splitId)
  );
}

function openTabInWorkspaceState(
  workspace: WorkspaceState,
  tab: WorkspaceTab,
  preferredPaneId: string | null,
  tabIndex?: number,
): WorkspaceState {
  const targetPaneId = workspace.panes.some(
    (pane) => pane.id === preferredPaneId,
  )
    ? preferredPaneId
    : (resolveWorkspaceOpenTargetPaneId(workspace, workspace.activePaneId) ??
      workspace.activePaneId ??
      workspace.panes[0]?.id ??
      null);

  if (!targetPaneId) {
    const pane = createPane(tab);
    return withActivePaneRoutingState(workspace, [pane], pane.id, {
      root: {
        type: "pane",
        paneId: pane.id,
      },
    });
  }

  const targetPane =
    workspace.panes.find((pane) => pane.id === targetPaneId) ?? null;
  if (
    targetPane &&
    tab.kind !== "controlPanel" &&
    !paneIsWorkspaceContentDestination(targetPane)
  ) {
    const alternatePaneId = findWorkspaceContentPaneId(
      workspace,
      targetPane.id,
    );
    if (alternatePaneId) {
      return addWorkspaceTabToPane(workspace, alternatePaneId, tab, tabIndex);
    }

    return openTabInAdjacentPane(workspace, targetPane.id, tab, "row", false);
  }

  if (
    targetPane &&
    tab.kind === "controlPanel" &&
    !paneContainsControlPanel(targetPane)
  ) {
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
  return withActivePaneRoutingState(
    workspace,
    [...workspace.panes, newPane],
    newPane.id,
    {
      root: insertPaneAdjacent(
        workspace.root,
        paneId,
        direction,
        newPane.id,
        placeBefore,
      ),
    },
  );
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

  // The generic opener creates a content pane only when the workspace has no
  // non-control-panel destination. Pane-count and tab-kind differences never
  // create implicit splits.
  return openTabInWorkspaceState(workspace, tab, preferredPaneId);
}

function syncPaneState(pane: WorkspacePane): WorkspacePane {
  const activeTab =
    pane.tabs.find((tab) => tab.id === pane.activeTabId) ??
    pane.tabs[0] ??
    null;
  const paneWithVisitHistory = withPrunedPaneTabVisitHistory(
    pane,
    activeTab?.id ?? null,
  );
  if (!activeTab) {
    return {
      ...paneWithVisitHistory,
      activeTabId: null,
      activeSessionId: null,
      viewMode: pane.lastSessionViewMode,
      sourcePath: null,
    };
  }

  if (activeTab.kind === "session") {
    const viewMode =
      pane.viewMode === "source" ? pane.lastSessionViewMode : pane.viewMode;
    const nextSessionViewMode = normalizeSessionViewMode(
      viewMode,
      pane.lastSessionViewMode,
    );
    return {
      ...paneWithVisitHistory,
      activeTabId: activeTab.id,
      activeSessionId: activeTab.sessionId,
      viewMode: nextSessionViewMode,
      lastSessionViewMode: nextSessionViewMode,
      sourcePath: null,
    };
  }

  if (activeTab.kind === "source") {
    return {
      ...paneWithVisitHistory,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(
        activeTab.originSessionId,
        pane.activeSessionId,
        pane.tabs,
      ),
      viewMode: "source",
      sourcePath: activeTab.path,
    };
  }

  if (activeTab.kind === "canvas") {
    return {
      ...paneWithVisitHistory,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(
        activeTab.originSessionId,
        pane.activeSessionId,
        pane.tabs,
      ),
      viewMode: "canvas",
      sourcePath: null,
    };
  }

  if (activeTab.kind === "orchestratorCanvas") {
    return {
      ...paneWithVisitHistory,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(
        activeTab.originSessionId,
        pane.activeSessionId,
        pane.tabs,
      ),
      viewMode: "orchestratorCanvas",
      sourcePath: null,
    };
  }

  if (activeTab.kind === "filesystem") {
    return {
      ...paneWithVisitHistory,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(
        activeTab.originSessionId,
        pane.activeSessionId,
        pane.tabs,
      ),
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
    return syncOriginOnlyPaneState(paneWithVisitHistory, activeTab);
  }

  if (activeTab.kind === "instructionDebugger") {
    return {
      ...paneWithVisitHistory,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(
        activeTab.originSessionId,
        pane.activeSessionId,
        pane.tabs,
      ),
      viewMode: "instructionDebugger",
      sourcePath: null,
    };
  }

  if (activeTab.kind === "terminal") {
    return {
      ...paneWithVisitHistory,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(
        activeTab.originSessionId,
        pane.activeSessionId,
        pane.tabs,
      ),
      viewMode: "terminal",
      sourcePath: null,
    };
  }

  if (activeTab.kind === "mailbox") {
    return {
      ...paneWithVisitHistory,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(
        activeTab.originSessionId,
        pane.activeSessionId,
        pane.tabs,
      ),
      viewMode: "mailbox",
      sourcePath: null,
    };
  }

  if (activeTab.kind === "responseBoard") {
    return {
      ...paneWithVisitHistory,
      activeTabId: activeTab.id,
      activeSessionId: resolveOriginSessionId(
        activeTab.originSessionId,
        pane.activeSessionId,
        pane.tabs,
      ),
      viewMode: "responseBoard",
      sourcePath: null,
    };
  }

  return {
    ...paneWithVisitHistory,
    activeTabId: activeTab.id,
    activeSessionId: resolveOriginSessionId(
      activeTab.originSessionId,
      pane.activeSessionId,
      pane.tabs,
    ),
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
  return (
    tabs.find((tab): tab is WorkspaceSessionTab => tab.kind === "session")
      ?.sessionId ?? null
  );
}

function isSessionTabActive(pane: WorkspacePane) {
  return pane.tabs.some(
    (tab) => tab.id === pane.activeTabId && tab.kind === "session",
  );
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
  const normalizedOriginSessionId =
    normalizeWorkspaceIdentifier(originSessionId);
  const normalizedOriginProjectId =
    normalizeWorkspaceIdentifier(originProjectId);
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceTerminalTab =>
        candidate.kind === "terminal" &&
        normalizeWorkspacePath(candidate.workdir) === normalizedWorkdir &&
        normalizeWorkspaceIdentifier(candidate.originSessionId) ===
          normalizedOriginSessionId &&
        normalizeWorkspaceIdentifier(candidate.originProjectId ?? null) ===
          normalizedOriginProjectId,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findMailboxTab(workspace: WorkspaceState, mailboxId: string) {
  const normalizedMailboxId = normalizeWorkspaceIdentifier(mailboxId);
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceMailboxTab =>
        candidate.kind === "mailbox" &&
        normalizeWorkspaceIdentifier(candidate.mailboxId) ===
          normalizedMailboxId,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

function findResponseBoardTab(workspace: WorkspaceState) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceResponseBoardTab =>
        candidate.kind === "responseBoard",
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
      (candidate): candidate is WorkspaceControlPanelTab =>
        candidate.kind === "controlPanel",
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
      (candidate): candidate is WorkspaceCanvasTab =>
        candidate.kind === "canvas",
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
      (candidate): candidate is WorkspaceSessionListTab =>
        candidate.kind === "sessionList",
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
      (candidate): candidate is WorkspaceProjectListTab =>
        candidate.kind === "projectList",
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
  const {
    originProjectId: _ignoredOriginProjectId,
    ...tabWithoutOriginProjectId
  } = tab;
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
    activeSessionId: resolveOriginSessionId(
      activeTab.originSessionId,
      pane.activeSessionId,
      pane.tabs,
    ),
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
    findWorkspaceContentPaneId(workspace, null) ??
    workspace.activePaneId ??
    workspace.panes[0]?.id ??
    null
  );
}

/**
 * Resolve the pane that represents the user's current workspace context.
 *
 * Pane-array and split-tree order are layout details, not focus history. Keep
 * the explicit MRU content pane ahead of all structural fallbacks. A pane that
 * has at least one content tab remains a content destination even while a
 * standalone Files, Git, Sessions, or Projects tab is selected. A pane made
 * only of ambient control surfaces is not a content destination.
 */
function findWorkspaceContentPaneId(
  workspace: WorkspaceState,
  excludePaneId: string | null,
) {
  const candidates = [workspace.lastContentPaneId, workspace.activePaneId];
  for (const candidateId of candidates) {
    if (!candidateId || candidateId === excludePaneId) {
      continue;
    }
    const candidate = workspace.panes.find((pane) => pane.id === candidateId);
    if (candidate && paneIsWorkspacePrimaryDestination(workspace, candidate)) {
      return candidate.id;
    }
  }

  const primaryPane = workspace.panes.find(
    (pane) =>
      pane.id !== excludePaneId &&
      paneIsWorkspacePrimaryDestination(workspace, pane),
  );
  if (primaryPane) {
    return primaryPane.id;
  }

  return (
    workspace.panes.find(
      (pane) =>
        pane.id !== excludePaneId && paneIsWorkspaceContentDestination(pane),
    )?.id ?? null
  );
}

function resolveWorkspaceOpenTargetPaneId(
  workspace: WorkspaceState,
  openerPaneId: string | null,
  originSessionId: string | null = null,
) {
  const openerPane = openerPaneId
    ? (workspace.panes.find((pane) => pane.id === openerPaneId) ?? null)
    : null;
  if (openerPane && paneIsWorkspacePrimaryDestination(workspace, openerPane)) {
    return openerPane.id;
  }

  for (const candidateId of [
    workspace.lastContentPaneId,
    workspace.activePaneId,
  ]) {
    if (!candidateId || candidateId === openerPane?.id) {
      continue;
    }
    const candidate = workspace.panes.find((pane) => pane.id === candidateId);
    if (candidate && paneIsWorkspacePrimaryDestination(workspace, candidate)) {
      return candidate.id;
    }
  }

  if (originSessionId) {
    const originPaneId =
      findSessionTab(workspace, originSessionId)?.paneId ?? null;
    if (originPaneId) {
      return originPaneId;
    }
  }

  return findWorkspaceContentPaneId(
    workspace,
    openerPane && !paneIsWorkspaceContentDestination(openerPane)
      ? openerPane.id
      : null,
  );
}

export function resolveWorkspaceViewerSplitAnchorPaneId(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
) {
  return (
    resolveWorkspaceOpenTargetPaneId(
      workspace,
      preferredPaneId,
      originSessionId,
    ) ?? preferredPaneId
  );
}

function resolveSessionOpenTargetPaneId(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
): { targetPaneId: string | null; splitAnchorPaneId: string | null } {
  const targetPaneId = resolveWorkspaceOpenTargetPaneId(
    workspace,
    preferredPaneId,
  );
  if (targetPaneId) {
    return { targetPaneId, splitAnchorPaneId: null };
  }

  const preferredPane = preferredPaneId
    ? (workspace.panes.find((pane) => pane.id === preferredPaneId) ?? null)
    : null;
  return {
    targetPaneId: null,
    splitAnchorPaneId:
      preferredPane && !paneIsWorkspaceContentDestination(preferredPane)
        ? preferredPane.id
        : null,
  };
}

function resolveViewerOpenTarget(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
  allowViewerSplit: boolean,
): {
  targetPaneId: string | null;
  splitAnchorPaneId: string | null;
  rememberTargetAsViewer?: boolean;
} {
  const preferredPane = preferredPaneId
    ? (workspace.panes.find((pane) => pane.id === preferredPaneId) ?? null)
    : null;

  if (
    preferredPane &&
    paneIsWorkspaceViewerDestination(workspace, preferredPane)
  ) {
    return { targetPaneId: preferredPane.id, splitAnchorPaneId: null };
  }

  for (const candidateId of [
    workspace.lastViewerPaneId,
    workspace.activePaneId,
  ]) {
    if (!candidateId || candidateId === preferredPane?.id) {
      continue;
    }
    const candidate = workspace.panes.find((pane) => pane.id === candidateId);
    if (candidate && paneIsWorkspaceViewerDestination(workspace, candidate)) {
      return { targetPaneId: candidate.id, splitAnchorPaneId: null };
    }
  }

  const existingViewerPane = workspace.panes.find((pane) =>
    paneIsWorkspaceViewerDestination(workspace, pane),
  );
  if (existingViewerPane) {
    return { targetPaneId: existingViewerPane.id, splitAnchorPaneId: null };
  }

  const contentPaneId = resolveWorkspaceOpenTargetPaneId(
    workspace,
    preferredPaneId,
    originSessionId,
  );
  if (contentPaneId) {
    const fallbackPaneId = findWorkspaceViewerFallbackPaneId(
      workspace,
      contentPaneId,
    );
    // A viewer lane should consume an existing second content pane before it
    // creates a third column. The active tab in that pane may be another
    // session: files and diffs are reference views, so adding them as tabs is
    // less disruptive than splitting an already-tiled conversation pane.
    if (fallbackPaneId) {
      return {
        targetPaneId: fallbackPaneId,
        splitAnchorPaneId: null,
        rememberTargetAsViewer: true,
      };
    }

    if (allowViewerSplit) {
      return { targetPaneId: null, splitAnchorPaneId: contentPaneId };
    }

    return {
      targetPaneId: contentPaneId,
      splitAnchorPaneId: null,
    };
  }

  const fallbackPaneId =
    preferredPane?.id ??
    workspace.activePaneId ??
    workspace.panes[0]?.id ??
    null;
  // `allowViewerSplit` gates the optional second lane beside existing content.
  // When the workspace contains only ambient control surfaces, the first
  // viewer still needs its own content pane: placing it in the control dock
  // would violate the stable pane-role boundary and cover the control panel.
  return { targetPaneId: null, splitAnchorPaneId: fallbackPaneId };
}

function workspaceWithRememberedViewerTarget(
  workspace: WorkspaceState,
  target: {
    targetPaneId: string | null;
    rememberTargetAsViewer?: boolean;
  },
) {
  return target.rememberTargetAsViewer && target.targetPaneId
    ? { ...workspace, lastViewerPaneId: target.targetPaneId }
    : workspace;
}

function findWorkspaceViewerFallbackPaneId(
  workspace: WorkspaceState,
  openerPaneId: string,
) {
  const paneLookup = new Map(workspace.panes.map((pane) => [pane.id, pane]));
  const paneOrder = flattenPaneOrder(workspace.root, paneLookup);
  const openerIndex = paneOrder.indexOf(openerPaneId);
  if (openerIndex < 0) {
    return null;
  }

  for (const preferPaneWithoutSession of [true, false]) {
    for (let distance = 1; distance < paneOrder.length; distance += 1) {
      for (const candidateIndex of [
        openerIndex + distance,
        openerIndex - distance,
      ]) {
        const candidateId = paneOrder[candidateIndex];
        const candidate = candidateId ? paneLookup.get(candidateId) : null;
        if (
          candidate &&
          paneIsWorkspaceContentDestination(candidate) &&
          (!preferPaneWithoutSession ||
            !candidate.tabs.some((tab) => tab.kind === "session"))
        ) {
          return candidate.id;
        }
      }
    }
  }

  return null;
}

function resolveCanvasOpenTargetPaneId(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
) {
  return resolveWorkspaceOpenTargetPaneId(
    workspace,
    preferredPaneId,
    originSessionId,
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
  const paneIndex = workspace.panes.findIndex(
    (pane) => pane.id === controlSurfacePaneId,
  );
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
    updatedTab = {
      ...activeTab,
      workdir,
      originSessionId: sessionId ?? activeTab.originSessionId,
      originProjectId: projectId,
    };
  } else if (activeTab.kind === "filesystem" && workdir) {
    updatedTab = {
      ...activeTab,
      rootPath: workdir,
      originSessionId: sessionId ?? activeTab.originSessionId,
      originProjectId: projectId,
    };
  } else if (
    activeTab.kind === "controlPanel" ||
    activeTab.kind === "sessionList" ||
    activeTab.kind === "projectList" ||
    activeTab.kind === "orchestratorList"
  ) {
    updatedTab = {
      ...activeTab,
      originSessionId: sessionId ?? activeTab.originSessionId,
      originProjectId: projectId,
    };
  }
  // Terminal tabs intentionally have no branch here — see the doc comment
  // above. Do NOT add one without also taking `TerminalPanel`'s history
  // keying and its project-scope dropdown into account.

  if (!updatedTab || updatedTab === activeTab) {
    return workspace;
  }

  const updatedTabs = pane.tabs.map((tab) =>
    tab.id === activeTab.id ? updatedTab! : tab,
  );
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

function resolveLastContentPaneId(
  workspace: WorkspaceState,
  panes: WorkspacePane[],
  activePaneId: string | null,
) {
  const activePane = activePaneId
    ? (panes.find((pane) => pane.id === activePaneId) ?? null)
    : null;
  if (activePane && paneIsWorkspacePrimaryDestination(workspace, activePane)) {
    return activePane.id;
  }

  for (const candidateId of [
    workspace.lastContentPaneId,
    workspace.activePaneId,
  ]) {
    if (!candidateId) {
      continue;
    }
    const candidate = panes.find((pane) => pane.id === candidateId);
    if (candidate && paneIsWorkspacePrimaryDestination(workspace, candidate)) {
      return candidate.id;
    }
  }

  const primaryPane = panes.find((pane) =>
    paneIsWorkspacePrimaryDestination(workspace, pane),
  );
  if (primaryPane) {
    return primaryPane.id;
  }
  if (activePane && paneIsWorkspaceContentDestination(activePane)) {
    return activePane.id;
  }
  return panes.find(paneIsWorkspaceContentDestination)?.id ?? null;
}

function resolveLastViewerPaneId(
  workspace: WorkspaceState,
  panes: WorkspacePane[],
  activePaneId: string | null,
) {
  const activePane = activePaneId
    ? (panes.find((pane) => pane.id === activePaneId) ?? null)
    : null;
  if (activePane && paneIsWorkspaceViewerDestination(workspace, activePane)) {
    return activePane.id;
  }

  for (const candidateId of [
    workspace.lastViewerPaneId,
    workspace.activePaneId,
  ]) {
    if (!candidateId) {
      continue;
    }
    const candidate = panes.find((pane) => pane.id === candidateId);
    if (candidate && paneIsWorkspaceViewerDestination(workspace, candidate)) {
      return candidate.id;
    }
  }

  return (
    panes.find((pane) => paneIsWorkspaceViewerDestination(workspace, pane))
      ?.id ?? null
  );
}

function withActivePaneRoutingState(
  workspace: WorkspaceState,
  panes: WorkspacePane[],
  activePaneId: string | null,
  overrides: { root?: WorkspaceNode | null } = {},
): WorkspaceState {
  return {
    ...workspace,
    ...overrides,
    panes,
    activePaneId,
    lastContentPaneId: resolveLastContentPaneId(workspace, panes, activePaneId),
    lastViewerPaneId: resolveLastViewerPaneId(workspace, panes, activePaneId),
  };
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
  "controlPanel",
  "sessionList",
  "projectList",
  "orchestratorList",
  "gitStatus",
  "filesystem",
]);

const WORKSPACE_VIEWER_TAB_KINDS: ReadonlySet<WorkspaceTab["kind"]> = new Set([
  "source",
  "diffPreview",
]);

function paneIsControlSurface(pane: WorkspacePane) {
  const activeTab = getActiveTab(pane);
  return activeTab ? CONTROL_SURFACE_KINDS.has(activeTab.kind) : false;
}

/**
 * Stable routing role for a pane, independent of whichever tab is selected.
 *
 * Ambient control surfaces may be stacked alongside a session, source, diff,
 * terminal, or canvas without turning that pane into the control dock. A pane
 * containing only ambient controls is not allowed to cover itself with new
 * workspace content; the opener will use another content pane or create one.
 */
function paneIsWorkspaceContentDestination(pane: WorkspacePane) {
  if (paneContainsControlPanel(pane)) {
    return false;
  }
  return (
    pane.tabs.length === 0 ||
    pane.tabs.some((tab) => !CONTROL_SURFACE_KINDS.has(tab.kind))
  );
}

function paneIsWorkspaceViewerDestination(
  workspace: WorkspaceState,
  pane: WorkspacePane,
) {
  const hasViewerTab = pane.tabs.some((tab) =>
    WORKSPACE_VIEWER_TAB_KINDS.has(tab.kind),
  );
  const hasPrimaryTab = pane.tabs.some(
    (tab) =>
      !CONTROL_SURFACE_KINDS.has(tab.kind) &&
      !WORKSPACE_VIEWER_TAB_KINDS.has(tab.kind),
  );
  return (
    paneIsWorkspaceContentDestination(pane) &&
    hasViewerTab &&
    (workspace.lastViewerPaneId === pane.id || !hasPrimaryTab)
  );
}

function paneIsWorkspacePrimaryDestination(
  workspace: WorkspaceState,
  pane: WorkspacePane,
) {
  return (
    paneIsWorkspaceContentDestination(pane) &&
    !paneIsWorkspaceViewerDestination(workspace, pane)
  );
}

function flattenPaneOrder(
  root: WorkspaceNode | null,
  paneLookup: Map<string, WorkspacePane>,
): string[] {
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

function cloneWorkspaceTab(
  tab: WorkspaceTab,
  transferredTabId: string = crypto.randomUUID(),
): WorkspaceTab {
  return {
    ...tab,
    id: transferredTabId,
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

function insertTabAtIndex(
  tabs: WorkspaceTab[],
  tab: WorkspaceTab,
  tabIndex: number,
): WorkspaceTab[] {
  const nextTabs = tabs.filter((candidate) => candidate.id !== tab.id);
  const nextTabIndex = clampIndex(tabIndex, 0, nextTabs.length);
  nextTabs.splice(nextTabIndex, 0, tab);
  return nextTabs;
}

function findContextualTargetPaneId(
  workspace: WorkspaceState,
  preferredPaneId: string | null,
  originSessionId: string | null,
  _tabKind: WorkspaceTab["kind"],
) {
  return resolveWorkspaceOpenTargetPaneId(
    workspace,
    preferredPaneId,
    originSessionId,
  );
}
function getActiveTab(pane: WorkspacePane) {
  return (
    pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? pane.tabs[0] ?? null
  );
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

function resolveOriginSessionId(
  originSessionId: string | null,
  activeSessionId: string | null,
  tabs: WorkspaceTab[],
) {
  return originSessionId ?? activeSessionId ?? firstSessionTabId(tabs);
}

function isSessionPaneViewMode(
  viewMode: PaneViewMode,
): viewMode is SessionPaneViewMode {
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

function pruneWorkspaceNode(
  node: WorkspaceNode | null,
  availablePaneIds: Set<string>,
): WorkspaceNode | null {
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

function removePaneFromTree(
  node: WorkspaceNode | null,
  paneId: string,
): WorkspaceNode | null {
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
    first: insertPaneAdjacent(
      node.first,
      paneId,
      direction,
      newPaneId,
      placeBefore,
    ),
    second: insertPaneAdjacent(
      node.second,
      paneId,
      direction,
      newPaneId,
      placeBefore,
    ),
  };
}

function updateSplitRatioInNode(
  node: WorkspaceNode,
  splitId: string,
  ratio: number,
): WorkspaceNode {
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
