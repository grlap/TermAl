import type { Dispatch, MutableRefObject, SetStateAction } from "react";
import {
  deleteProject,
  fetchGitDiff,
  pauseOrchestratorInstance,
  resumeOrchestratorInstance,
  stopOrchestratorInstance,
  type GitDiffRequestPayload,
  type GitDiffSection,
  type OpenPathOptions,
  type StateResponse,
} from "./api";
import { appTestHooks } from "./app-test-hooks";
import { ALL_PROJECTS_FILTER_ID } from "./project-filters";
import {
  buildGitDiffPreviewRequestKey,
  getErrorMessage,
  pendingGitDiffPreviewChangeType,
  pendingGitDiffPreviewSummary,
  type DraftImageAttachment,
} from "./app-utils";
import { syncComposerDraftForSession } from "./session-store";
import {
  activatePane,
  closeWorkspaceTab,
  CONTROL_SURFACE_KINDS,
  findNearestControlSurfacePaneId,
  findNearestSessionPaneId,
  openCanvasInWorkspaceState,
  openDiffPreviewInWorkspaceState,
  openFilesystemInWorkspaceState,
  openGitStatusInWorkspaceState,
  openInstructionDebuggerInWorkspaceState,
  openOrchestratorCanvasInWorkspaceState,
  openOrchestratorListInWorkspaceState,
  openProjectListInWorkspaceState,
  openSessionInWorkspaceState,
  openSessionListInWorkspaceState,
  openSourceInWorkspaceState,
  openTerminalInWorkspaceState,
  removeCanvasSessionCard,
  rescopeControlSurfacePane,
  setCanvasZoom,
  setPaneSourcePath,
  setPaneViewMode,
  splitPane,
  updateGitDiffPreviewTabInWorkspaceState,
  upsertCanvasSessionCard,
  type SessionPaneViewMode,
  type WorkspacePane,
  type WorkspaceState,
  type WorkspaceTab,
} from "./workspace";
import { resolveWorkspaceTabProjectId } from "./workspace-queries";
import type {
  OrchestratorRuntimeAction,
  StandaloneControlSurfaceViewState,
} from "./app-shell-internals";
import type { AgentType, DiffMessage, Project, Session } from "./types";

type PendingScrollToBottomRequest = {
  sessionId: string;
  token: number;
} | null;

type UseAppWorkspaceActionsParams = {
  workspace: WorkspaceState;
  workspaceRef: MutableRefObject<WorkspaceState>;
  paneLookup: Map<string, WorkspacePane>;
  sessionLookup: Map<string, Session>;
  projectLookup: Map<string, Project>;
  isMountedRef: MutableRefObject<boolean>;
  gitDiffPreviewRefreshVersionsRef: MutableRefObject<Map<string, number>>;
  attemptedGitDiffDocumentContentRestoreKeysRef: MutableRefObject<Set<string>>;
  newSessionAgent: AgentType;
  newSessionModel: string;
  createSessionPaneId: string | null;
  createSessionProjectId: string;
  closePendingSessionRename: (restoreFocus?: boolean) => void;
  setKillRevealSessionId: Dispatch<SetStateAction<string | null>>;
  setSelectedProjectId: Dispatch<SetStateAction<string>>;
  setWorkspace: Dispatch<SetStateAction<WorkspaceState>>;
  setStandaloneControlSurfaceViewStateByTabId: Dispatch<
    SetStateAction<Record<string, StandaloneControlSurfaceViewState>>
  >;
  setDraftsBySessionId: Dispatch<SetStateAction<Record<string, string>>>;
  draftsBySessionIdRef: MutableRefObject<Record<string, string>>;
  draftAttachmentsBySessionIdRef: MutableRefObject<
    Record<string, DraftImageAttachment[]>
  >;
  forceSessionScrollToBottomRef: MutableRefObject<
    Record<string, true | undefined>
  >;
  setPendingScrollToBottomRequest: Dispatch<
    SetStateAction<PendingScrollToBottomRequest>
  >;
  setRequestError: Dispatch<SetStateAction<string | null>>;
  setPendingOrchestratorActionById: Dispatch<
    SetStateAction<Record<string, OrchestratorRuntimeAction | undefined>>
  >;
  setIsCreateSessionOpen: Dispatch<SetStateAction<boolean>>;
  applyControlPanelLayout: (
    nextWorkspace: WorkspaceState,
    side?: "left" | "right",
  ) => WorkspaceState;
  markSessionTabsForBottomAfterWorkspaceRebuild: (
    nextWorkspace: WorkspaceState,
  ) => void;
  reportRequestError: (
    error: unknown,
    options?: { message?: string },
  ) => void;
  adoptState: (nextState: StateResponse) => boolean;
  handleNewSession: (options: {
    agent: AgentType;
    model: string;
    preferredPaneId: string | null;
    projectSelectionId: string;
  }) => Promise<boolean>;
  openCreateSessionDialog: (
    preferredPaneId?: string | null,
    defaultProjectSelectionId?: string | null,
  ) => void;
};

type UseAppWorkspaceActionsReturn = {
  handleCreateSessionDialogSubmit: () => Promise<void>;
  handleSidebarSessionClick: (
    sessionId: string,
    preferredPaneId?: string | null,
    syncControlPanelProject?: boolean,
  ) => void;
  handleOpenConversationFromDiff: (
    sessionId: string,
    preferredPaneId?: string | null,
  ) => void;
  handleInsertReviewIntoPrompt: (
    sessionId: string,
    preferredPaneId: string | null,
    prompt: string,
  ) => void;
  handleScrollToBottomRequestHandled: (token: number) => void;
  handlePaneActivate: (paneId: string) => void;
  handlePaneTabSelect: (paneId: string, tabId: string) => void;
  handleCloseTab: (paneId: string, tabId: string) => void;
  handleSplitPane: (paneId: string, direction: "row" | "column") => void;
  handleDraftChange: (sessionId: string, nextValue: string) => void;
  handlePaneViewModeChange: (
    paneId: string,
    viewMode: SessionPaneViewMode,
  ) => void;
  handlePaneSourcePathChange: (paneId: string, path: string) => void;
  handleOpenSourceTab: (
    paneId: string,
    path: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: OpenPathOptions,
  ) => void;
  handleOpenDiffPreviewTab: (
    paneId: string,
    message: DiffMessage,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  handleOpenGitStatusDiffPreviewTab: (
    paneId: string,
    request: GitDiffRequestPayload,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: {
      openInNewTab?: boolean;
      sectionId?: GitDiffSection;
    },
  ) => Promise<void>;
  handleOpenFilesystemTab: (
    paneId: string,
    rootPath: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  handleOpenGitStatusTab: (
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  handleOpenTerminalTab: (
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  handleOpenSessionListTab: (
    paneId: string,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  handleOpenProjectListTab: (
    paneId: string,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  handleOpenCanvasTab: (
    paneId: string,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  handleOpenOrchestratorListTab: (
    paneId: string,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  handleOpenOrchestratorCanvasTab: (
    paneId: string,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: {
      startMode?: "new" | null;
      templateId?: string | null;
    },
  ) => void;
  handleUpsertCanvasSessionCard: (
    canvasTabId: string,
    sessionId: string,
    position: { x: number; y: number },
  ) => void;
  handleRemoveCanvasSessionCard: (
    canvasTabId: string,
    sessionId: string,
  ) => void;
  handleSetCanvasZoom: (canvasTabId: string, zoom: number) => void;
  handleOrchestratorStateUpdated: (state: StateResponse) => void;
  handleOrchestratorRuntimeAction: (
    instanceId: string,
    action: OrchestratorRuntimeAction,
  ) => Promise<void>;
  handleOpenInstructionDebuggerTab: (
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  handleProjectMenuStartSession: (
    paneId: string | null,
    projectId: string,
  ) => void;
  handleProjectMenuRemoveProject: (project: Project) => Promise<void>;
};

export function useAppWorkspaceActions({
  workspace,
  workspaceRef,
  paneLookup,
  sessionLookup,
  projectLookup,
  isMountedRef,
  gitDiffPreviewRefreshVersionsRef,
  attemptedGitDiffDocumentContentRestoreKeysRef,
  newSessionAgent,
  newSessionModel,
  createSessionPaneId,
  createSessionProjectId,
  closePendingSessionRename,
  setKillRevealSessionId,
  setSelectedProjectId,
  setWorkspace,
  setStandaloneControlSurfaceViewStateByTabId,
  setDraftsBySessionId,
  draftsBySessionIdRef,
  draftAttachmentsBySessionIdRef,
  forceSessionScrollToBottomRef,
  setPendingScrollToBottomRequest,
  setRequestError,
  setPendingOrchestratorActionById,
  setIsCreateSessionOpen,
  applyControlPanelLayout,
  markSessionTabsForBottomAfterWorkspaceRebuild,
  reportRequestError,
  adoptState,
  handleNewSession,
  openCreateSessionDialog,
}: UseAppWorkspaceActionsParams): UseAppWorkspaceActionsReturn {
  function requestScrollToBottom(sessionId: string) {
    setPendingScrollToBottomRequest({
      sessionId,
      token: Date.now() + Math.random(),
    });
  }

  function resetRemovedProjectSelection(projectId: string) {
    setSelectedProjectId((current) =>
      current === projectId ? ALL_PROJECTS_FILTER_ID : current,
    );
    setStandaloneControlSurfaceViewStateByTabId((current) => {
      let changed = false;
      const nextState: Record<string, StandaloneControlSurfaceViewState> = {};

      for (const [tabId, viewState] of Object.entries(current)) {
        if (viewState.projectId === projectId) {
          changed = true;
          nextState[tabId] = {
            ...viewState,
            projectId: ALL_PROJECTS_FILTER_ID,
          };
          continue;
        }

        nextState[tabId] = viewState;
      }

      return changed ? nextState : current;
    });
    setWorkspace((current) => {
      let changed = false;
      const panes = current.panes.map((pane) => {
        let paneChanged = false;
        const tabs = pane.tabs.map((tab): WorkspaceTab => {
          if ("originProjectId" in tab && tab.originProjectId === projectId) {
            paneChanged = true;
            return {
              ...tab,
              originProjectId: null,
            };
          }

          return tab;
        });

        if (!paneChanged) {
          return pane;
        }

        changed = true;
        return {
          ...pane,
          tabs,
        };
      });

      return changed ? { ...current, panes } : current;
    });
  }

  async function handleCreateSessionDialogSubmit() {
    const created = await handleNewSession({
      agent: newSessionAgent,
      model: newSessionModel,
      preferredPaneId: createSessionPaneId,
      projectSelectionId: createSessionProjectId,
    });

    if (created && isMountedRef.current) {
      setIsCreateSessionOpen(false);
    }
  }

  function handleSidebarSessionClick(
    sessionId: string,
    preferredPaneId: string | null = null,
    syncControlPanelProject = true,
  ) {
    const session = sessionLookup.get(sessionId);
    closePendingSessionRename();
    setKillRevealSessionId(null);
    if (syncControlPanelProject) {
      setSelectedProjectId(session?.projectId ?? ALL_PROJECTS_FILTER_ID);
    }
    requestScrollToBottom(sessionId);
    setWorkspace((current) =>
      applyControlPanelLayout(
        openSessionInWorkspaceState(
          current,
          sessionId,
          preferredPaneId ?? current.activePaneId,
        ),
      ),
    );
  }

  function handleOpenConversationFromDiff(
    sessionId: string,
    preferredPaneId: string | null = null,
  ) {
    handleSidebarSessionClick(sessionId, preferredPaneId);
  }

  function handleInsertReviewIntoPrompt(
    sessionId: string,
    preferredPaneId: string | null,
    prompt: string,
  ) {
    const nextPrompt = prompt.trim();
    handleOpenConversationFromDiff(sessionId, preferredPaneId);
    if (!nextPrompt) {
      return;
    }

    const existingDraft = draftsBySessionIdRef.current[sessionId] ?? "";
    const nextValue =
      existingDraft.trim().length > 0
        ? `${existingDraft.trimEnd()}\n\n${nextPrompt}`
        : nextPrompt;
    if (existingDraft !== nextValue) {
      draftsBySessionIdRef.current = {
        ...draftsBySessionIdRef.current,
        [sessionId]: nextValue,
      };
      syncComposerDraftForSession({
        sessionId,
        committedDraft: nextValue,
        draftAttachments:
          draftAttachmentsBySessionIdRef.current[sessionId] ?? [],
      });
    }

    setDraftsBySessionId((current) => {
      const existingDraft = current[sessionId] ?? "";
      const nextValue =
        existingDraft.trim().length > 0
          ? `${existingDraft.trimEnd()}\n\n${nextPrompt}`
          : nextPrompt;
      if (existingDraft === nextValue) {
        return current;
      }

      return {
        ...current,
        [sessionId]: nextValue,
      };
    });
  }

  function handleScrollToBottomRequestHandled(token: number) {
    setPendingScrollToBottomRequest((current) =>
      current?.token === token ? null : current,
    );
  }

  function handlePaneActivate(paneId: string) {
    setWorkspace((current) => activatePane(current, paneId));
  }

  function handlePaneTabSelect(paneId: string, tabId: string) {
    const pane = paneLookup.get(paneId);
    const tab = pane?.tabs.find((candidate) => candidate.id === tabId);

    if (tab?.kind === "controlPanel") {
      const nearestSessionPaneId = findNearestSessionPaneId(workspace, paneId);
      const nearestSessionPane = nearestSessionPaneId
        ? (paneLookup.get(nearestSessionPaneId) ?? null)
        : null;
      const nearestSessionTab = nearestSessionPane
        ? (nearestSessionPane.tabs.find(
            (candidate) => candidate.id === nearestSessionPane.activeTabId,
          ) ??
            nearestSessionPane.tabs[0] ??
            null)
        : null;
      const nearestSession =
        nearestSessionTab?.kind === "session"
          ? (sessionLookup.get(nearestSessionTab.sessionId) ?? null)
          : null;
      if (nearestSession) {
        const projectId = nearestSession.projectId ?? null;
        setSelectedProjectId(
          projectId && projectLookup.has(projectId)
            ? projectId
            : ALL_PROJECTS_FILTER_ID,
        );
      }

      setWorkspace((current) => {
        const next = activatePane(current, paneId, tabId);
        if (!nearestSession) {
          return next;
        }

        return rescopeControlSurfacePane(
          next,
          paneId,
          nearestSession.id,
          nearestSession.projectId ?? null,
          nearestSession.workdir ?? null,
        );
      });
      return;
    }

    if (tab && CONTROL_SURFACE_KINDS.has(tab.kind)) {
      const nearestSessionPaneId = findNearestSessionPaneId(workspace, paneId);
      const nearestSessionPane = nearestSessionPaneId
        ? (paneLookup.get(nearestSessionPaneId) ?? null)
        : null;
      const nearestSessionTab = nearestSessionPane
        ? (nearestSessionPane.tabs.find(
            (candidate) => candidate.id === nearestSessionPane.activeTabId,
          ) ??
            nearestSessionPane.tabs[0] ??
            null)
        : null;
      const nearestSession =
        nearestSessionTab?.kind === "session"
          ? (sessionLookup.get(nearestSessionTab.sessionId) ?? null)
          : null;
      if (nearestSession) {
        setSelectedProjectId(
          nearestSession.projectId &&
            projectLookup.has(nearestSession.projectId)
            ? nearestSession.projectId
            : ALL_PROJECTS_FILTER_ID,
        );
      }

      setWorkspace((current) => {
        const next = activatePane(current, paneId, tabId);
        if (!nearestSession) {
          return next;
        }

        return rescopeControlSurfacePane(
          next,
          paneId,
          nearestSession.id,
          nearestSession.projectId ?? null,
          nearestSession.workdir ?? null,
        );
      });
      return;
    }

    const nearestControlSurface = findNearestControlSurfacePaneId(
      workspace,
      paneId,
    );
    if (nearestControlSurface) {
      const session =
        tab?.kind === "session" ? sessionLookup.get(tab.sessionId) : null;
      const nearestPane = paneLookup.get(nearestControlSurface);
      const nearestActiveTab = nearestPane?.tabs.find(
        (candidate) => candidate.id === nearestPane.activeTabId,
      );
      const nearestIsDockedControlPanel =
        nearestActiveTab?.kind === "controlPanel";

      if (session) {
        const projectId = session.projectId ?? null;
        if (
          nearestIsDockedControlPanel &&
          projectId &&
          projectLookup.has(projectId)
        ) {
          setSelectedProjectId(projectId);
        }
      } else {
        const projectId = resolveWorkspaceTabProjectId(tab, sessionLookup);
        if (
          projectId &&
          projectLookup.has(projectId) &&
          nearestIsDockedControlPanel
        ) {
          setSelectedProjectId(projectId);
        }
      }
    }

    if (tab?.kind === "session") {
      forceSessionScrollToBottomRef.current[tab.sessionId] = true;
    }

    setWorkspace((current) => activatePane(current, paneId, tabId));
  }

  function handleCloseTab(paneId: string, tabId: string) {
    setWorkspace((current) =>
      applyControlPanelLayout(closeWorkspaceTab(current, paneId, tabId)),
    );
  }

  function handleSplitPane(paneId: string, direction: "row" | "column") {
    markSessionTabsForBottomAfterWorkspaceRebuild(workspaceRef.current);
    setWorkspace((current) =>
      applyControlPanelLayout(splitPane(current, paneId, direction)),
    );
  }

  function handleDraftChange(sessionId: string, nextValue: string) {
    if ((draftsBySessionIdRef.current[sessionId] ?? "") !== nextValue) {
      draftsBySessionIdRef.current = {
        ...draftsBySessionIdRef.current,
        [sessionId]: nextValue,
      };
      syncComposerDraftForSession({
        sessionId,
        committedDraft: nextValue,
        draftAttachments:
          draftAttachmentsBySessionIdRef.current[sessionId] ?? [],
      });
    }
    setDraftsBySessionId((current) => {
      if ((current[sessionId] ?? "") === nextValue) {
        return current;
      }

      return {
        ...current,
        [sessionId]: nextValue,
      };
    });
  }

  function handlePaneViewModeChange(
    paneId: string,
    viewMode: SessionPaneViewMode,
  ) {
    if (viewMode === "session") {
      const pane = paneLookup.get(paneId);
      const activeTab = pane?.tabs.find(
        (candidate) => candidate.id === pane.activeTabId,
      );
      if (activeTab?.kind === "session") {
        requestScrollToBottom(activeTab.sessionId);
      }
    }

    setWorkspace((current) => setPaneViewMode(current, paneId, viewMode));
  }

  function handlePaneSourcePathChange(paneId: string, path: string) {
    setWorkspace((current) => setPaneSourcePath(current, paneId, path));
  }

  function handleOpenSourceTab(
    paneId: string,
    path: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: OpenPathOptions,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openSourceInWorkspaceState(
          current,
          path,
          paneId,
          originSessionId,
          originProjectId,
          {
            line: options?.line,
            column: options?.column,
            openInNewTab: options?.openInNewTab,
          },
        ),
      ),
    );
  }

  function handleOpenDiffPreviewTab(
    paneId: string,
    message: DiffMessage,
    originSessionId: string | null,
    originProjectId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openDiffPreviewInWorkspaceState(
          current,
          {
            changeType: message.changeType,
            changeSetId: message.changeSetId ?? `change-${message.id}`,
            diff: message.diff,
            diffMessageId: message.id,
            filePath: message.filePath,
            language: message.language ?? null,
            originSessionId,
            originProjectId,
            summary: message.summary,
          },
          paneId,
        ),
      ),
    );
  }

  async function handleOpenGitStatusDiffPreviewTab(
    paneId: string,
    request: GitDiffRequestPayload,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: {
      openInNewTab?: boolean;
      sectionId?: GitDiffSection;
    },
  ) {
    const requestKey = buildGitDiffPreviewRequestKey(
      paneId,
      request,
      Boolean(options?.openInNewTab),
    );
    const currentVersion =
      (gitDiffPreviewRefreshVersionsRef.current.get(requestKey) ?? 0) + 1;
    gitDiffPreviewRefreshVersionsRef.current.set(requestKey, currentVersion);
    attemptedGitDiffDocumentContentRestoreKeysRef.current.add(requestKey);
    const gitSectionId = options?.sectionId ?? request.sectionId;
    const pendingTab = {
      changeType: pendingGitDiffPreviewChangeType(request.statusCode),
      changeSetId: null,
      diff: "",
      documentEnrichmentNote: null,
      documentContent: null,
      diffMessageId: requestKey,
      filePath: request.path,
      gitSectionId,
      language: null,
      originSessionId,
      originProjectId,
      summary: pendingGitDiffPreviewSummary(gitSectionId, request.path),
      gitDiffRequestKey: requestKey,
      gitDiffRequest: request,
      isLoading: true,
      loadError: null,
    };

    setWorkspace((current) => {
      const opened = openDiffPreviewInWorkspaceState(
        current,
        pendingTab,
        paneId,
        options?.openInNewTab
          ? {
              openInNewTab: true,
            }
          : {
              reuseActiveViewerTab: true,
            },
      );
      return applyControlPanelLayout(
        updateGitDiffPreviewTabInWorkspaceState(opened, requestKey, (tab) => ({
          ...tab,
          ...pendingTab,
          id: tab.id,
        })),
      );
    });

    try {
      const diffPreview = await fetchGitDiff(request);
      if (
        !isMountedRef.current ||
        gitDiffPreviewRefreshVersionsRef.current.get(requestKey) !==
          currentVersion
      ) {
        return;
      }
      setWorkspace((current) =>
        applyControlPanelLayout(
          updateGitDiffPreviewTabInWorkspaceState(
            current,
            requestKey,
            (tab) => ({
              ...tab,
              changeType: diffPreview.changeType,
              changeSetId: diffPreview.changeSetId ?? null,
              diff: diffPreview.diff,
              documentEnrichmentNote:
                diffPreview.documentEnrichmentNote ?? null,
              documentContent: diffPreview.documentContent ?? null,
              filePath: diffPreview.filePath ?? tab.filePath,
              gitSectionId,
              language: diffPreview.language ?? null,
              summary: diffPreview.summary,
              isLoading: false,
              loadError: null,
            }),
          ),
        ),
      );
    } catch (error) {
      if (
        !isMountedRef.current ||
        gitDiffPreviewRefreshVersionsRef.current.get(requestKey) !==
          currentVersion
      ) {
        return;
      }
      const errorMessage = getErrorMessage(error);
      setWorkspace((current) =>
        applyControlPanelLayout(
          updateGitDiffPreviewTabInWorkspaceState(
            current,
            requestKey,
            (tab) => ({
              ...tab,
              diff: "",
              documentEnrichmentNote: null,
              documentContent: null,
              summary: `Failed to load ${gitSectionId} changes in ${request.path}`,
              isLoading: false,
              loadError: errorMessage,
            }),
          ),
        ),
      );
      throw error;
    }
  }

  function handleOpenFilesystemTab(
    paneId: string,
    rootPath: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openFilesystemInWorkspaceState(
          current,
          rootPath,
          paneId,
          originSessionId,
          originProjectId,
        ),
      ),
    );
  }

  function handleOpenGitStatusTab(
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openGitStatusInWorkspaceState(
          current,
          workdir,
          paneId,
          originSessionId,
          originProjectId,
        ),
      ),
    );
  }

  function handleOpenTerminalTab(
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openTerminalInWorkspaceState(
          current,
          workdir,
          paneId,
          originSessionId,
          originProjectId,
        ),
      ),
    );
  }

  function handleOpenSessionListTab(
    paneId: string,
    originSessionId: string | null,
    originProjectId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openSessionListInWorkspaceState(
          current,
          paneId,
          originSessionId,
          originProjectId,
        ),
      ),
    );
  }

  function handleOpenProjectListTab(
    paneId: string,
    originSessionId: string | null,
    originProjectId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openProjectListInWorkspaceState(
          current,
          paneId,
          originSessionId,
          originProjectId,
        ),
      ),
    );
  }

  function handleOpenCanvasTab(
    paneId: string,
    originSessionId: string | null,
    originProjectId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openCanvasInWorkspaceState(
          current,
          paneId,
          originSessionId,
          originProjectId,
        ),
      ),
    );
  }

  function handleOpenOrchestratorListTab(
    paneId: string,
    originSessionId: string | null,
    originProjectId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openOrchestratorListInWorkspaceState(
          current,
          paneId,
          originSessionId,
          originProjectId,
        ),
      ),
    );
  }

  function handleOpenOrchestratorCanvasTab(
    paneId: string,
    originSessionId: string | null,
    originProjectId: string | null,
    options: {
      startMode?: "new" | null;
      templateId?: string | null;
    } = {},
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openOrchestratorCanvasInWorkspaceState(
          current,
          paneId,
          originSessionId,
          originProjectId,
          options,
        ),
      ),
    );
  }

  function handleUpsertCanvasSessionCard(
    canvasTabId: string,
    sessionId: string,
    position: { x: number; y: number },
  ) {
    setWorkspace((current) =>
      upsertCanvasSessionCard(current, canvasTabId, {
        sessionId,
        x: position.x,
        y: position.y,
      }),
    );
  }

  function handleRemoveCanvasSessionCard(
    canvasTabId: string,
    sessionId: string,
  ) {
    setWorkspace((current) =>
      removeCanvasSessionCard(current, canvasTabId, sessionId),
    );
  }

  function handleSetCanvasZoom(canvasTabId: string, zoom: number) {
    setWorkspace((current) => setCanvasZoom(current, canvasTabId, zoom));
  }

  function handleOrchestratorStateUpdated(state: StateResponse) {
    adoptState(state);
  }

  async function handleOrchestratorRuntimeAction(
    instanceId: string,
    action: OrchestratorRuntimeAction,
  ) {
    setRequestError(null);
    setPendingOrchestratorActionById((current) => ({
      ...current,
      [instanceId]: action,
    }));

    try {
      let state: StateResponse;
      switch (action) {
        case "pause":
          state = await pauseOrchestratorInstance(instanceId);
          break;
        case "resume":
          state = await resumeOrchestratorInstance(instanceId);
          break;
        case "stop":
          state = await stopOrchestratorInstance(instanceId);
          break;
      }

      if (!isMountedRef.current) {
        return;
      }

      handleOrchestratorStateUpdated(state);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }

      reportRequestError(error);
    } finally {
      if (isMountedRef.current) {
        setPendingOrchestratorActionById((current) => {
          const { [instanceId]: _discarded, ...rest } = current;
          return rest;
        });
      }
    }
  }

  function handleOpenInstructionDebuggerTab(
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openInstructionDebuggerInWorkspaceState(
          current,
          workdir,
          paneId,
          originSessionId,
          originProjectId,
        ),
      ),
    );
  }

  function handleProjectMenuStartSession(
    paneId: string | null,
    projectId: string,
  ) {
    openCreateSessionDialog(paneId, projectId);
  }

  async function handleProjectMenuRemoveProject(project: Project) {
    const confirmed = window.confirm(
      `Remove "${project.name}" from TermAl? Existing sessions stay in All projects. Files on disk are not deleted.`,
    );
    if (!confirmed) {
      return;
    }

    try {
      const state = await deleteProject(project.id);
      if (!isMountedRef.current) {
        return;
      }
      appTestHooks?.onDeleteProjectPostAwaitPath?.("resolve");
      adoptState(state);
      resetRemovedProjectSelection(project.id);
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      appTestHooks?.onDeleteProjectPostAwaitPath?.("reject");
      reportRequestError(error);
    }
  }

  return {
    handleCreateSessionDialogSubmit,
    handleSidebarSessionClick,
    handleOpenConversationFromDiff,
    handleInsertReviewIntoPrompt,
    handleScrollToBottomRequestHandled,
    handlePaneActivate,
    handlePaneTabSelect,
    handleCloseTab,
    handleSplitPane,
    handleDraftChange,
    handlePaneViewModeChange,
    handlePaneSourcePathChange,
    handleOpenSourceTab,
    handleOpenDiffPreviewTab,
    handleOpenGitStatusDiffPreviewTab,
    handleOpenFilesystemTab,
    handleOpenGitStatusTab,
    handleOpenTerminalTab,
    handleOpenSessionListTab,
    handleOpenProjectListTab,
    handleOpenCanvasTab,
    handleOpenOrchestratorListTab,
    handleOpenOrchestratorCanvasTab,
    handleUpsertCanvasSessionCard,
    handleRemoveCanvasSessionCard,
    handleSetCanvasZoom,
    handleOrchestratorStateUpdated,
    handleOrchestratorRuntimeAction,
    handleOpenInstructionDebuggerTab,
    handleProjectMenuStartSession,
    handleProjectMenuRemoveProject,
  };
}
