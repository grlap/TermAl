import {
  startTransition,
  useCallback,
  useDeferredValue,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type DragEvent as ReactDragEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { flushSync } from "react-dom";
import { isDialogBackdropDismissMouseDown } from "./dialog-backdrop-dismiss";
import { DialogCloseIcon } from "./message-card-icons";
import {
  deleteProject,
  fetchGitDiff,
  fetchGitStatus,
  fetchState,
  isBackendUnavailableError,
  pauseOrchestratorInstance,
  resumeOrchestratorInstance,
  stopOrchestratorInstance,
  type GitDiffRequestPayload,
  type GitDiffSection,
  type OpenPathOptions,
  type StateResponse,
  updateAppSettings,
} from "./api";
import { AgentIcon } from "./agent-icon";
import {
  createSessionModelHint,
  defaultNewSessionModel,
  describeProjectScope,
  resolveControlPanelWorkspaceRoot,
  resolveRemoteConfig,
  remoteBadgeLabel,
  usesSessionModelPicker,
  CLAUDE_EFFORT_OPTIONS,
  CODEX_REASONING_EFFORT_OPTIONS,
  NEW_SESSION_MODEL_OPTIONS,
  type ComboboxOption,
} from "./session-model-utils";

import {
  ThemePreferencesPanel,
  AppearancePreferencesPanel,
  MarkdownPreferencesPanel,
  RemotePreferencesPanel,
  ClaudeApprovalsPreferencesPanel,
  CodexPromptPreferencesPanel,
  ThemedCombobox,
  CURSOR_MODE_OPTIONS,
  GEMINI_APPROVAL_OPTIONS,
} from "./preferences-panels";
import { SettingsDialogShell } from "./preferences/SettingsDialogShell";
import { SettingsTabBar } from "./preferences/SettingsTabBar";
import type { PreferencesTabId } from "./preferences/preferences-tabs";
import { resolveStandaloneControlPanelDockWidthRatio } from "./control-panel-layout";
import {
  getWorkspaceSplitResizeBounds,
  resolveControlSurfaceSectionIdForWorkspaceTab,
  resolveWorkspaceTabProjectId,
  workspaceContainsOnlyControlPanel,
} from "./workspace-queries";
import {
  buildControlSurfaceSessionListEntries,
  buildControlSurfaceSessionListState,
  createControlPanelSectionLauncherTab,
  formatSessionOrchestratorGroupName,
} from "./control-surface-state";
import {
  collectGitDiffPreviewRefreshes,
  collectRestoredGitDiffDocumentContentRefreshes,
} from "./git-diff-refresh";
import {
  BACKEND_UNAVAILABLE_ISSUE_DETAIL,
  type BackendConnectionState,
} from "./backend-connection";
import { createInitialWorkspaceBootstrap } from "./initial-workspace-bootstrap";
import { useAppPreferencesState } from "./app-preferences-state";
import { useAppWorkspaceLayout } from "./app-workspace-layout";
import { useAppLiveState } from "./app-live-state";
import { useAppSessionActions } from "./app-session-actions";
import { useAppDragResize } from "./app-drag-resize";
import { useAppDialogState } from "./app-dialog-state";
import { useAppWorkspaceActions } from "./app-workspace-actions";
import { useAppControlPanelState } from "./app-control-panel-state";
import { AppControlSurface } from "./AppControlSurface";
import { AppDialogs } from "./AppDialogs";
import { appTestHooks } from "./app-test-hooks";
import { ProjectListSection } from "./ProjectListSection";
import { ALL_PROJECTS_FILTER_ID } from "./project-filters";
import { EmptyState } from "./EmptyState";
import { WorkspaceNodeView } from "./WorkspaceNodeView";

import {
  LOCAL_REMOTE_ID,
  createBuiltinLocalRemote,
  isLocalRemoteId,
  remoteConnectionLabel,
  remoteDisplayName,
  resolveProjectRemoteId,
} from "./remotes";
import {
  ControlPanelConnectionIndicator,
  WorkspaceSwitcher,
} from "./workspace-shell-controls";
import type { RuntimeAction } from "./runtime-action-button";
import { OrchestratorRuntimeActionButton } from "./OrchestratorRuntimeActionButton";
import type {
  ControlPanelSectionId,
  ControlPanelSurfaceHandle,
} from "./panels/ControlPanelSurface";
import { FileSystemPanel } from "./panels/FileSystemPanel";
import { GitStatusPanel } from "./panels/GitStatusPanel";
import { OrchestratorTemplateLibraryPanel } from "./panels/OrchestratorTemplateLibraryPanel";
import { OrchestratorTemplatesPanel } from "./panels/OrchestratorTemplatesPanel";
import {
  pruneTerminalPanelHistory,
} from "./panels/TerminalPanel";
import {
  buildSessionListSearchResultFromIndex,
  buildSessionSearchIndex,
  type SessionListSearchResult,
} from "./session-find";
import type {
  AgentReadiness,
  AgentType,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  CodexState,
  CursorMode,
  DiffMessage,
  ExhaustiveValueCoverage,
  GeminiApprovalMode,
  Message,
  PendingPrompt,
  OrchestratorInstance,
  Project,
  RemoteConfig,
  SandboxMode,
  Session,
} from "./types";
import {
  activatePane,
  closeWorkspaceTab,
  CONTROL_SURFACE_KINDS,
  DEFAULT_CONTROL_PANEL_DOCK_WIDTH_RATIO,
  dockControlPanelAtWorkspaceEdge,
  ensureControlPanelInWorkspaceState,
  findNearestControlSurfacePaneId,
  findNearestSessionPaneId,
  getSplitRatio,
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
  placeSessionDropInWorkspaceState,
  placeDraggedTab,
  placeExternalTab,
  reconcileWorkspaceState,
  removeCanvasSessionCard,
  rescopeControlSurfacePane,
  setCanvasZoom,
  setPaneSourcePath,
  setPaneViewMode,
  splitPane,
  updateGitDiffPreviewTabInWorkspaceState,
  updateSplitRatio,
  upsertCanvasSessionCard,
  type SessionPaneViewMode,
  type TabDropPlacement,
  type WorkspaceState,
  type WorkspaceTab,
} from "./workspace";
import {
  ensureWorkspaceViewId,
  type ControlPanelSide,
} from "./workspace-storage";
import {
  attachSessionDragData,
  readSessionDragData,
} from "./session-drag";
import {
  MARKDOWN_STYLES,
  MARKDOWN_THEMES,
  STYLES,
  THEMES,
  clampDensityPreference,
  clampEditorFontSizePreference,
  clampFontSizePreference,
  type DiagramLook,
  type DiagramPalette,
  type DiagramThemeOverrideMode,
  type MarkdownStyleId,
  type MarkdownThemeId,
  type StyleId,
  type ThemeId,
} from "./themes";
import type { MonacoAppearance } from "./monaco";
import {
  countSessionsByFilter,
  filterSessionsByListFilter,
  type SessionListFilter,
} from "./session-list-filter";
import {
  TAB_DRAG_CHANNEL_NAME,
  attachWorkspaceTabDragData,
  createWorkspaceTabDrag,
  isWorkspaceTabDragChannelMessage,
  readWorkspaceTabDragData,
  type WorkspaceTabDrag,
  type WorkspaceTabDragChannelMessage,
} from "./tab-drag";
import {
  clamp,
  buildGitDiffPreviewRequestKey,
  getErrorMessage,
  isHexColorDark,
  pendingGitDiffPreviewChangeType,
  pendingGitDiffPreviewSummary,
  primaryModifierLabel,
  pruneSessionFlags,
  readNavigatorOnline,
  releaseDraftAttachments,
  type DraftImageAttachment,
  type SessionAgentCommandMap,
  type SessionFlagMap,
} from "./app-utils";
import {
  CREATE_SESSION_WORKSPACE_ID,
  NEW_SESSION_AGENT_OPTIONS,
  NEW_SESSION_AGENT_OPTIONS_EXHAUSTIVE,
  PENDING_KILL_CLOSE_DELAY_MS,
  PENDING_SESSION_RENAME_CLOSE_DELAY_MS,
  TAB_DRAG_STALE_TIMEOUT_MS,
  type OrchestratorRuntimeAction,
  type PendingSessionRename,
  type SessionConversationItem,
  type SessionErrorMap,
  type SessionNoticeMap,
  type StandaloneControlSurfaceViewState,
} from "./app-shell-internals";

export default function App() {
  const [workspaceViewId] = useState(() => ensureWorkspaceViewId());
  const [projects, setProjects] = useState<Project[]>([]);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [orchestrators, setOrchestrators] = useState<OrchestratorInstance[]>(
    [],
  );
  const [codexState, setCodexState] = useState<CodexState>({});
  const [agentReadiness, setAgentReadiness] = useState<AgentReadiness[]>([]);
  const initialWorkspaceBootstrapRef = useRef<ReturnType<
    typeof createInitialWorkspaceBootstrap
  > | null>(null);
  if (!initialWorkspaceBootstrapRef.current) {
    initialWorkspaceBootstrapRef.current =
      createInitialWorkspaceBootstrap(workspaceViewId);
  }
  const initialWorkspaceBootstrap = initialWorkspaceBootstrapRef.current!;
  const [controlPanelSide, setControlPanelSide] = useState<ControlPanelSide>(
    initialWorkspaceBootstrap.controlPanelSide,
  );
  const [workspace, setWorkspace] = useState<WorkspaceState>(
    initialWorkspaceBootstrap.workspace,
  );
  const [isWorkspaceSwitcherOpen, setIsWorkspaceSwitcherOpen] = useState(false);
  const [draftsBySessionId, setDraftsBySessionId] = useState<
    Record<string, string>
  >({});
  const [draftAttachmentsBySessionId, setDraftAttachmentsBySessionId] =
    useState<Record<string, DraftImageAttachment[]>>({});
  const [newSessionAgent, setNewSessionAgent] = useState<AgentType>("Codex");
  const [newSessionModelByAgent, setNewSessionModelByAgent] = useState<
    Record<AgentType, string>
  >(() => ({
    Claude: defaultNewSessionModel("Claude"),
    Codex: defaultNewSessionModel("Codex"),
    Cursor: defaultNewSessionModel("Cursor"),
    Gemini: defaultNewSessionModel("Gemini"),
  }));
  const [isLoading, setIsLoading] = useState(true);
  const [isCreating, setIsCreating] = useState(false);
  const [sendingSessionIds, setSendingSessionIds] = useState<SessionFlagMap>(
    {},
  );
  const [stoppingSessionIds, setStoppingSessionIds] = useState<SessionFlagMap>(
    {},
  );
  const [pendingOrchestratorActionById, setPendingOrchestratorActionById] =
    useState<Record<string, OrchestratorRuntimeAction | undefined>>({});
  const [killingSessionIds, setKillingSessionIds] = useState<SessionFlagMap>(
    {},
  );
  const [killRevealSessionId, setKillRevealSessionId] = useState<string | null>(
    null,
  );
  const [pendingKillSessionId, setPendingKillSessionId] = useState<
    string | null
  >(null);
  const [pendingSessionRename, setPendingSessionRename] =
    useState<PendingSessionRename | null>(null);
  const [updatingSessionIds, setUpdatingSessionIds] = useState<SessionFlagMap>(
    {},
  );
  const [refreshingSessionModelOptionIds, setRefreshingSessionModelOptionIds] =
    useState<SessionFlagMap>({});
  const [sessionModelOptionErrors, setSessionModelOptionErrors] =
    useState<SessionErrorMap>({});
  const [agentCommandsBySessionId, setAgentCommandsBySessionId] =
    useState<SessionAgentCommandMap>({});
  const [
    refreshingAgentCommandSessionIds,
    setRefreshingAgentCommandSessionIds,
  ] = useState<SessionFlagMap>({});
  const [agentCommandErrors, setAgentCommandErrors] = useState<SessionErrorMap>(
    {},
  );
  const [sessionSettingNotices, setSessionSettingNotices] =
    useState<SessionNoticeMap>({});
  const [requestError, setRequestError] = useState<string | null>(null);
  const [
    backendInlineRequestErrorMessage,
    setBackendInlineRequestErrorMessage,
  ] = useState<string | null>(null);
  const [backendConnectionIssueDetail, setBackendConnectionIssueDetail] =
    useState<string | null>(null);
  const initialBackendConnectionState: BackendConnectionState =
    readNavigatorOnline() ? "connecting" : "offline";
  const [backendConnectionState, setBackendConnectionStateRaw] =
    useState<BackendConnectionState>(initialBackendConnectionState);
  const backendConnectionStateRef = useRef<BackendConnectionState>(
    initialBackendConnectionState,
  );
  const setBackendConnectionState = useCallback(
    (next: BackendConnectionState) => {
      // Write the ref eagerly so same-tick online/offline handlers observe the
      // next connection state before React commits.
      backendConnectionStateRef.current = next;
      setBackendConnectionStateRaw(next);
    },
    [],
  );
  const [sessionListFilter, setSessionListFilter] =
    useState<SessionListFilter>("all");
  const [sessionListSearchQuery, setSessionListSearchQuery] = useState("");
  const [selectedProjectId, setSelectedProjectId] = useState<string>(
    ALL_PROJECTS_FILTER_ID,
  );
  const [newProjectRootPath, setNewProjectRootPath] = useState("");
  const [newProjectRemoteId, setNewProjectRemoteId] =
    useState<string>(LOCAL_REMOTE_ID);
  const [isCreatingProject, setIsCreatingProject] = useState(false);
  const {
    themeId,
    setThemeId,
    styleId,
    setStyleId,
    markdownThemeId,
    setMarkdownThemeId,
    markdownStyleId,
    setMarkdownStyleId,
    diagramThemeOverrideMode,
    setDiagramThemeOverrideMode,
    diagramLook,
    setDiagramLook,
    diagramPalette,
    setDiagramPalette,
    fontSizePx,
    setFontSizePx,
    editorFontSizePx,
    setEditorFontSizePx,
    densityPercent,
    setDensityPercent,
    defaultCodexSandboxMode,
    setDefaultCodexSandboxMode,
    defaultCodexApprovalPolicy,
    setDefaultCodexApprovalPolicy,
    defaultCodexReasoningEffort,
    setDefaultCodexReasoningEffort,
    defaultClaudeApprovalMode,
    setDefaultClaudeApprovalMode,
    defaultClaudeEffort,
    setDefaultClaudeEffort,
    defaultCursorMode,
    setDefaultCursorMode,
    defaultGeminiApprovalMode,
    setDefaultGeminiApprovalMode,
    remoteConfigs,
    setRemoteConfigs,
  } = useAppPreferencesState(initialWorkspaceBootstrap);
  const [isCreateSessionOpen, setIsCreateSessionOpen] = useState(false);
  const [createSessionPaneId, setCreateSessionPaneId] = useState<string | null>(
    null,
  );
  const [createSessionProjectId, setCreateSessionProjectId] = useState<string>(
    CREATE_SESSION_WORKSPACE_ID,
  );
  const [isCreateProjectOpen, setIsCreateProjectOpen] = useState(false);
  const [controlPanelFilesystemRoot, setControlPanelFilesystemRoot] = useState<
    string | null
  >(null);
  const [controlPanelGitWorkdir, setControlPanelGitWorkdir] = useState<
    string | null
  >(null);
  const [controlPanelGitStatusCount, setControlPanelGitStatusCount] =
    useState(0);
  const [
    standaloneControlSurfaceViewStateByTabId,
    setStandaloneControlSurfaceViewStateByTabId,
  ] = useState<Record<string, StandaloneControlSurfaceViewState>>({});
  const [
    collapsedSessionOrchestratorIdsBySurfaceId,
    setCollapsedSessionOrchestratorIdsBySurfaceId,
  ] = useState<Record<string, string[]>>({});
  const [pendingScrollToBottomRequest, setPendingScrollToBottomRequest] =
    useState<{
      sessionId: string;
      token: number;
    } | null>(null);
  const [windowId] = useState(() => crypto.randomUUID());
  const gitDiffPreviewRefreshVersionsRef = useRef<Map<string, number>>(
    new Map(),
  );
  const pendingGitDiffDocumentContentRestoreKeysRef = useRef<Set<string>>(
    new Set(),
  );
  const attemptedGitDiffDocumentContentRestoreKeysRef = useRef<Set<string>>(
    new Set(),
  );
  const backendInlineRequestErrorMessageRef = useRef<string | null>(null);
  const draftsRef = useRef<Record<string, string>>({});
  const draftAttachmentsRef = useRef<Record<string, DraftImageAttachment[]>>(
    {},
  );
  const isMountedRef = useRef(true);
  // Self-chained safety-net poll (see the sendMessage path). A previous
  // implementation used `setInterval`, which stacks overlapping fires when
  // a slow `/api/state` response exceeds the interval â€” on large transcripts
  // that caused the backend to be hit by multiple concurrent full-state
  // serializations. The current implementation delegates to
  // `startActivePromptPoll`, which chains `setTimeout` so the next poll is
  // only scheduled after the previous one completes and enforces a
  // deadline-based hard cap. The ref holds the cancel function so both
  // unmount cleanup and the "new prompt replaces prior poll" path in
  // `handleSend` can stop an in-progress chain.
  const activePromptPollCancelRef = useRef<(() => void) | null>(null);
  const sessionListSearchInputRef = useRef<HTMLInputElement>(null);
  const confirmedUnknownModelSendsRef = useRef<Set<string>>(new Set());
  const refreshingSessionModelOptionIdsRef = useRef<SessionFlagMap>({});
  const refreshingAgentCommandSessionIdsRef = useRef<SessionFlagMap>({});
  const controlPanelSurfaceRef = useRef<ControlPanelSurfaceHandle | null>(null);
  const lastDerivedControlPanelFilesystemRootRef = useRef<string | null>(null);
  const lastDerivedControlPanelGitWorkdirRef = useRef<string | null>(null);
  const workspaceSwitcherRef = useRef<HTMLDivElement | null>(null);
  const sessionsRef = useRef<Session[]>([]);
  const workspaceRef = useRef(workspace);
  const codexStateRef = useRef(codexState);
  const agentReadinessRef = useRef(agentReadiness);
  const projectsRef = useRef(projects);
  const orchestratorsRef = useRef(orchestrators);
  const latestStateRevisionRef = useRef<number | null>(null);
  // The `serverInstanceId` the client last adopted. Paired with
  // `latestStateRevisionRef` as the server-restart detector: when an
  // incoming snapshot carries a non-empty `serverInstanceId` that
  // differs from this ref, the server has just restarted and its
  // revision counter rewound to whatever SQLite had, so the client
  // accepts the snapshot regardless of the monotonic revision guard.
  // Updated in lockstep with `latestStateRevisionRef` inside
  // `adoptState` / `adoptCreatedSessionResponse` / `adoptFetchedSession`.
  const lastSeenServerInstanceIdRef = useRef<string | null>(null);
  // Populated by the live-state hook's transport useEffect on
  // mount and reset to a no-op on cleanup. App.tsx owns the ref
  // identity because `reportRequestError` and
  // `handleRetryBackendConnection` (which invoke these) are
  // declared before the hook is called.
  const requestBackendReconnectRef = useRef<() => void>(() => {});
  const requestActionRecoveryResyncRef = useRef<
    (options?: { openSessionId?: string; paneId?: string | null }) => void
  >(() => {});
  const paneShouldStickToBottomRef = useRef<
    Record<string, boolean | undefined>
  >({});
  const paneScrollPositionsRef = useRef<
    Record<string, Record<string, { top: number; shouldStick: boolean }>>
  >({});
  const paneContentSignaturesRef = useRef<
    Record<string, Record<string, string>>
  >({});
  const forceSessionScrollToBottomRef = useRef<
    Record<string, true | undefined>
  >({});

  const projectLookup = useMemo(
    () => new Map(projects.map((project) => [project.id, project])),
    [projects],
  );
  const agentReadinessByAgent = useMemo(
    () => new Map(agentReadiness.map((entry) => [entry.agent, entry])),
    [agentReadiness],
  );
  const sessionLookup = useMemo(
    () => new Map(sessions.map((session) => [session.id, session])),
    [sessions],
  );
  const paneLookup = useMemo(
    () => new Map(workspace.panes.map((pane) => [pane.id, pane])),
    [workspace.panes],
  );
  const activePane =
    workspace.panes.find((pane) => pane.id === workspace.activePaneId) ??
    workspace.panes[0] ??
    null;
  const activeSession = activePane?.activeSessionId
    ? (sessionLookup.get(activePane.activeSessionId) ?? null)
    : null;
  const openSessionIds = useMemo(
    () =>
      new Set(
        workspace.panes.flatMap((pane) =>
          pane.tabs.flatMap((tab) =>
            tab.kind === "session" ? [tab.sessionId] : [],
          ),
        ),
      ),
    [workspace.panes],
  );
  const workspaceHasOnlyControlPanel = useMemo(
    () => workspaceContainsOnlyControlPanel(workspace),
    [workspace],
  );
  const workspaceHasControlPanelTab = useMemo(
    () =>
      workspace.panes.some((pane) =>
        pane.tabs.some((tab) => tab.kind === "controlPanel"),
      ),
    [workspace.panes],
  );
  const workspaceShowsInlineControlPanelStatus = useMemo(
    () =>
      workspace.panes.some((pane) => {
        const activeTab =
          pane.tabs.find((tab) => tab.id === pane.activeTabId) ??
          pane.tabs[0] ??
          null;
        return (
          activeTab?.kind === "controlPanel" ||
          activeTab?.kind === "orchestratorList" ||
          activeTab?.kind === "sessionList" ||
          activeTab?.kind === "projectList"
        );
      }),
    [workspace.panes],
  );
  function setBackendInlineRequestError(message: string | null) {
    backendInlineRequestErrorMessageRef.current = message;
    setBackendInlineRequestErrorMessage(message);
  }

  useEffect(() => {
    if (requestError === null) {
      setBackendInlineRequestError(null);
    }
  }, [requestError]);
  const controlPanelInlineIssueDetail = backendConnectionIssueDetail;
  const requestErrorShownInline =
    requestError !== null &&
    workspaceShowsInlineControlPanelStatus &&
    backendInlineRequestErrorMessage !== null &&
    requestError === backendInlineRequestErrorMessage;

  function reportRequestError(
    error: unknown,
    options?: {
      message?: string;
    },
  ) {
    const message =
      options?.message ??
      (typeof error === "string" ? error : getErrorMessage(error));
    setRequestError(message);
    if (typeof error === "string" || !isBackendUnavailableError(error)) {
      setBackendInlineRequestError(null);
      return;
    }

    if (!readNavigatorOnline()) {
      // Preserve the inline error marker so clearRecoveredBackendRequestError
      // can match and clear the requestError when the browser reconnects.
      setBackendInlineRequestError(message);
      setBackendConnectionIssueDetail(null);
      setBackendConnectionState("offline");
      return;
    }

    // Incompatible backend serving HTML â€” show the restart instruction in the
    // request error toast but do not trigger auto-reconnect since the backend
    // IS reachable and reconnect success would immediately clear the guidance.
    // The toast is NOT cleared by transport-level recovery (SSE open, state
    // adoption) because those cannot prove the specific incompatible route is
    // fixed. It clears when the exact route that raised it succeeds (e.g.
    // workspace layout re-fetch via workspaceLayoutRestartErrorMessageRef),
    // through page reload (real TermAl restart), the next successful user
    // action (which calls setRequestError(null)), or a new error overwriting
    // it.
    if (error.restartRequired) {
      setBackendInlineRequestError(null);
      return;
    }

    setBackendInlineRequestError(message);
    setBackendConnectionIssueDetail(BACKEND_UNAVAILABLE_ISSUE_DETAIL);
    // Do NOT mutate backendConnectionState here. The connection badge reflects
    // the SSE transport state, and a one-off action 502 does not mean the SSE
    // stream is down. Mutating it to "reconnecting" would leave a permanent
    // badge because the action-recovery resync only clears error text â€” only
    // EventSource.onopen / confirmReconnectRecoveryFromLiveEvent restore
    // "connected", and those won't fire when the stream never went down.
    //
    // The inline request error + issue detail are enough to surface the error.
    // The action-recovery probe will clear both on success.
    requestActionRecoveryResyncRef.current();
  }

  const clearRecoveredBackendRequestError = useCallback(() => {
    const inlineRequestErrorMessage =
      backendInlineRequestErrorMessageRef.current;
    if (inlineRequestErrorMessage === null) {
      return;
    }

    setRequestError((current) =>
      current === inlineRequestErrorMessage ? null : current,
    );
    setBackendInlineRequestError(null);
  }, []);

  function handleRetryBackendConnection() {
    if (!readNavigatorOnline()) {
      return;
    }

    setBackendConnectionIssueDetail(null);
    clearRecoveredBackendRequestError();
    setBackendConnectionState(
      latestStateRevisionRef.current === null ? "connecting" : "reconnecting",
    );
    requestBackendReconnectRef.current();
  }

  const {
    isWorkspaceLayoutReady,
    workspaceSummaries,
    workspaceSummariesRef,
    setWorkspaceSummaries,
    isWorkspaceSwitcherLoading,
    workspaceSwitcherError,
    deletingWorkspaceIds,
    ignoreFetchedWorkspaceLayoutRef,
    workspaceLayoutLoadPendingRef,
    pendingWorkspaceLayoutSaveRef,
    flushWorkspaceLayoutSaveRef,
    refreshWorkspaceSummaries,
    flushPendingWorkspaceLayoutSave,
    handleWorkspaceSwitcherToggle,
    handleOpenWorkspaceHere,
    handleOpenNewWorkspaceHere,
    handleOpenNewWorkspaceWindow,
    handleDeleteWorkspace,
  } = useAppWorkspaceLayout({
    workspaceViewId,
    workspace,
    setWorkspace,
    controlPanelSide,
    setControlPanelSide,
    preferences: {
      themeId,
      styleId,
      markdownThemeId,
      markdownStyleId,
      diagramThemeOverrideMode,
      diagramLook,
      diagramPalette,
      fontSizePx,
      editorFontSizePx,
      densityPercent,
    },
    setPreferences: {
      setThemeId,
      setStyleId,
      setMarkdownThemeId,
      setMarkdownStyleId,
      setDiagramThemeOverrideMode,
      setDiagramLook,
      setDiagramPalette,
      setFontSizePx,
      setEditorFontSizePx,
      setDensityPercent,
    },
    setIsWorkspaceSwitcherOpen,
    setRequestError,
    isMountedRef,
    clearRecoveredBackendRequestError,
    setBackendConnectionState,
    reportRequestError,
    applyControlPanelLayout,
  });

  const {
    adoptState,
    adoptCreatedSessionResponse,
    syncPreferencesFromState,
    hydratedSessionIdsRef,
    hydratingSessionIdsRef,
    forceAdoptNextStateEventRef,
    workspaceFilesChangedEvent,
    workspaceFilesChangedEventBufferRef,
    workspaceFilesChangedEventFlushTimeoutRef,
    resetWorkspaceFilesChangedEventGate,
  } = useAppLiveState({
    adoptionRefs: {
      isMountedRef,
      latestStateRevisionRef,
      lastSeenServerInstanceIdRef,
      sessionsRef,
      draftsBySessionIdRef: draftsRef,
      draftAttachmentsBySessionIdRef: draftAttachmentsRef,
      codexStateRef,
      agentReadinessRef,
      projectsRef,
      orchestratorsRef,
      workspaceSummariesRef,
      refreshingAgentCommandSessionIdsRef,
      confirmedUnknownModelSendsRef,
    },
    stateSetters: {
      setSessions,
      setWorkspace,
      setCodexState,
      setAgentReadiness,
      setProjects,
      setOrchestrators,
      setWorkspaceSummaries,
      setDraftsBySessionId,
      setDraftAttachmentsBySessionId,
      setSendingSessionIds,
      setStoppingSessionIds,
      setKillingSessionIds,
      setKillRevealSessionId,
      setPendingKillSessionId,
      setPendingSessionRename,
      setUpdatingSessionIds,
      setAgentCommandsBySessionId,
      setRefreshingAgentCommandSessionIds,
      setAgentCommandErrors,
      setSessionSettingNotices,
      setSelectedProjectId,
      setIsLoading,
      setBackendConnectionIssueDetail,
      setBackendConnectionState,
    },
    preferenceSetters: {
      setDefaultCodexReasoningEffort,
      setDefaultClaudeApprovalMode,
      setDefaultClaudeEffort,
      setRemoteConfigs,
    },
    applyControlPanelLayout,
    clearRecoveredBackendRequestError,
    reportRequestError,
    requestBackendReconnectRef,
    requestActionRecoveryResyncRef,
    activeSession,
  });

  const selectedProject =
    selectedProjectId === ALL_PROJECTS_FILTER_ID
      ? null
      : (projectLookup.get(selectedProjectId) ?? null);
  const newProjectUsesLocalRemote = isLocalRemoteId(newProjectRemoteId);
  const {
    handleSend,
    handleDraftAttachmentsAdd,
    handleDraftAttachmentRemove,
    handleNewSession,
    handleCloneSessionFromExisting,
    handleCreateProject,
    handlePickProjectRoot,
    handleApprovalDecision,
    handleUserInputSubmit,
    handleMcpElicitationSubmit,
    handleCodexAppRequestSubmit,
    handleCancelQueuedPrompt,
    handleStopSession,
    executeKillSession,
    handleRenameSession,
    handleSessionSettingsChange,
    handleRefreshSessionModelOptions,
    handleForkCodexThread,
    handleArchiveCodexThread,
    handleUnarchiveCodexThread,
    handleCompactCodexThread,
    handleRollbackCodexThread,
    handleRefreshAgentCommands,
  } = useAppSessionActions({
    lookups: {
      sessionLookup,
      projectLookup,
      agentReadinessByAgent,
      activeSession,
      workspace,
    },
    newProjectRootPath,
    newProjectRemoteId,
    newProjectUsesLocalRemote,
    defaults: {
      defaultCodexApprovalPolicy,
      defaultCodexReasoningEffort,
      defaultCodexSandboxMode,
      defaultClaudeApprovalMode,
      defaultClaudeEffort,
      defaultCursorMode,
      defaultGeminiApprovalMode,
    },
    refs: {
      isMountedRef,
      sessionsRef,
      draftsBySessionIdRef: draftsRef,
      draftAttachmentsBySessionIdRef: draftAttachmentsRef,
      confirmedUnknownModelSendsRef,
      activePromptPollCancelRef,
      refreshingSessionModelOptionIdsRef,
      refreshingAgentCommandSessionIdsRef,
    },
    setters: {
      setSessions,
      setWorkspace,
      setRequestError,
      setIsCreating,
      setSendingSessionIds,
      setDraftsBySessionId,
      setDraftAttachmentsBySessionId,
      setIsCreatingProject,
      setNewProjectRootPath,
      setNewProjectRemoteId,
      setSelectedProjectId,
      setStoppingSessionIds,
      setKillingSessionIds,
      setUpdatingSessionIds,
      setSessionSettingNotices,
      setRefreshingSessionModelOptionIds,
      setSessionModelOptionErrors,
      setAgentCommandsBySessionId,
      setRefreshingAgentCommandSessionIds,
      setAgentCommandErrors,
    },
    adoptState,
    adoptCreatedSessionResponse,
    applyControlPanelLayout,
    reportRequestError,
    requestActionRecoveryResync: (options) =>
      requestActionRecoveryResyncRef.current(options),
  });
  const {
    isSettingsOpen,
    setIsSettingsOpen,
    settingsTab,
    setSettingsTab,
    pendingKillSession,
    pendingKillPopoverStyle,
    pendingKillPopoverRef,
    pendingKillConfirmButtonRef,
    handleKillSession,
    confirmKillSession,
    clearPendingKillCloseTimeout,
    schedulePendingKillConfirmationClose,
    closePendingKillConfirmation,
    pendingSessionRenameSession,
    pendingSessionRenameDraft,
    setPendingSessionRenameDraft,
    pendingSessionRenameValue,
    isPendingSessionRenameSubmitting,
    isPendingSessionRenameCreating,
    isPendingSessionRenameKilling,
    pendingSessionRenameStyle,
    pendingSessionRenamePopoverRef,
    pendingSessionRenameInputRef,
    handleSessionRenameRequest,
    confirmSessionRename,
    handlePendingSessionRenameNew,
    handlePendingSessionRenameKill,
    clearPendingSessionRenameCloseTimeout,
    schedulePendingSessionRenameClose,
    closePendingSessionRename,
    openCreateSessionDialog,
    openCreateProjectDialog,
  } = useAppDialogState({
    selectedProjectId,
    activeSession,
    workspaceActivePaneId: workspace.activePaneId,
    projectLookup,
    sessionLookup,
    updatingSessionIds,
    killingSessionIds,
    isCreating,
    killRevealSessionId,
    setKillRevealSessionId,
    pendingKillSessionId,
    setPendingKillSessionId,
    pendingSessionRename,
    setPendingSessionRename,
    setIsCreateSessionOpen,
    setCreateSessionPaneId,
    setCreateSessionProjectId,
    setIsCreateProjectOpen,
    setNewProjectRemoteId,
    clearRequestError: () => setRequestError(null),
    executeKillSession,
    handleRenameSession,
    handleCloneSessionFromExisting,
  });
  const {
    remoteLookup,
    localRemoteConfig,
    enabledProjectRemotes,
    newProjectSelectedRemote,
    createProjectRemoteOptions,
    newSessionModelOptions,
    createSessionSelectedProject,
    createSessionWorkspaceProject,
    createSessionEffectiveProject,
    createSessionSelectedRemote,
    createSessionProjectOptions,
    controlPanelProjectOptions,
    createSessionProjectHint,
    createSessionUsesRemoteProject,
    createSessionProjectSelectionError,
    createSessionUsesSessionModelPicker,
    createSessionAgentReadiness,
    createSessionBlocked,
    projectScopedSessions,
    controlPanelContextSession,
    controlPanelSessionId,
    sessionFilterCounts,
    hasSessionListSearch,
    sessionListSearchResults,
    filteredSessions,
    projectSessionCounts,
  } = useAppControlPanelState({
    remoteConfigs,
    activeSession,
    selectedProject,
    selectedProjectId,
    projects,
    sessions,
    projectLookup,
    paneLookup,
    sessionLookup,
    workspace,
    newProjectRemoteId,
    setNewProjectRemoteId,
    createSessionProjectId,
    setCreateSessionProjectId,
    newSessionAgent,
    agentReadinessByAgent,
    sessionListFilter,
    sessionListSearchQuery,
    controlPanelFilesystemRoot,
    setControlPanelFilesystemRoot,
    controlPanelGitWorkdir,
    setControlPanelGitWorkdir,
    setControlPanelGitStatusCount,
    lastDerivedControlPanelFilesystemRootRef,
    lastDerivedControlPanelGitWorkdirRef,
  });
  const newSessionModel =
    newSessionModelByAgent[newSessionAgent] ??
    defaultNewSessionModel(newSessionAgent);
  const {
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
  } = useAppWorkspaceActions({
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
    draftsBySessionIdRef: draftsRef,
    draftAttachmentsBySessionIdRef: draftAttachmentsRef,
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
  });
  const activeTheme = THEMES.find((theme) => theme.id === themeId) ?? THEMES[0];
  const activeStyle = STYLES.find((style) => style.id === styleId) ?? STYLES[0];
  const activeMarkdownTheme =
    MARKDOWN_THEMES.find((theme) => theme.id === markdownThemeId) ??
    MARKDOWN_THEMES[0];
  const activeMarkdownStyle =
    MARKDOWN_STYLES.find((style) => style.id === markdownStyleId) ??
    MARKDOWN_STYLES[0];
  const editorAppearance: MonacoAppearance = isHexColorDark(
    activeTheme.swatches[0],
  )
    ? "dark"
    : "light";
  function focusSessionListSearch(selectAll = false) {
    controlPanelSurfaceRef.current?.selectSection("sessions");
    window.requestAnimationFrame(() => {
      const input = sessionListSearchInputRef.current;
      if (!input) {
        return;
      }

      input.focus();
      if (selectAll) {
        input.select();
      }
    });
  }

  function applyControlPanelLayout(
    nextWorkspace: WorkspaceState,
    side: "left" | "right" = controlPanelSide,
  ) {
    const preferredControlPanelWidthRatio =
      workspaceHasOnlyControlPanel &&
      !workspaceContainsOnlyControlPanel(nextWorkspace)
        ? resolveStandaloneControlPanelDockWidthRatio(
            DEFAULT_CONTROL_PANEL_DOCK_WIDTH_RATIO,
          )
        : null;

    return dockControlPanelAtWorkspaceEdge(
      ensureControlPanelInWorkspaceState(nextWorkspace),
      side,
      preferredControlPanelWidthRatio,
    );
  }

  function markSessionTabsForBottomAfterWorkspaceRebuild(
    workspaceState: WorkspaceState,
    options?: {
      sessionIds?: readonly string[];
      tabs?: readonly WorkspaceTab[];
    },
  ) {
    const sessionIds = new Set<string>();
    for (const pane of workspaceState.panes) {
      for (const tab of pane.tabs) {
        if (tab.kind === "session") {
          sessionIds.add(tab.sessionId);
        }
      }
    }

    for (const sessionId of options?.sessionIds ?? []) {
      sessionIds.add(sessionId);
    }
    for (const tab of options?.tabs ?? []) {
      if (tab.kind === "session") {
        sessionIds.add(tab.sessionId);
      }
    }

    for (const sessionId of sessionIds) {
      forceSessionScrollToBottomRef.current[sessionId] = true;
    }
  }

  const {
    activeDraggedTab,
    getKnownWorkspaceTabDrag,
    handleSplitResizeStart,
    handleTabDragStart,
    handleTabDragEnd,
    handleControlPanelLauncherDragStart,
    handleControlPanelLauncherDragEnd,
    handleTabDrop,
  } = useAppDragResize({
    windowId,
    workspace,
    paneLookup,
    controlPanelSide,
    setControlPanelSide,
    setWorkspace,
    applyControlPanelLayout,
    workspaceLayoutLoadPendingRef,
    ignoreFetchedWorkspaceLayoutRef,
    markSessionTabsForBottomAfterWorkspaceRebuild,
  });

  async function persistAppPreferences(payload: {
    defaultCodexReasoningEffort?: CodexReasoningEffort;
    defaultClaudeApprovalMode?: ClaudeApprovalMode;
    defaultClaudeEffort?: ClaudeEffortLevel;
    remotes?: RemoteConfig[];
  }) {
    try {
      const state = await updateAppSettings(payload);
      if (!isMountedRef.current) {
        return;
      }
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
      try {
        const state = await fetchState();
        if (!isMountedRef.current) {
          return;
        }
        syncPreferencesFromState(state);
        adoptState(state);
      } catch {
        // Keep the optimistic selection until the next successful sync.
      }
    }
  }

  function handleDefaultCodexReasoningEffortChange(
    nextValue: CodexReasoningEffort,
  ) {
    if (nextValue === defaultCodexReasoningEffort) {
      return;
    }

    setDefaultCodexReasoningEffort(nextValue);
    void persistAppPreferences({ defaultCodexReasoningEffort: nextValue });
  }

  function handleDefaultClaudeApprovalModeChange(
    nextValue: ClaudeApprovalMode,
  ) {
    if (nextValue === defaultClaudeApprovalMode) {
      return;
    }

    setDefaultClaudeApprovalMode(nextValue);
    void persistAppPreferences({ defaultClaudeApprovalMode: nextValue });
  }

  function handleDefaultClaudeEffortChange(nextValue: ClaudeEffortLevel) {
    if (nextValue === defaultClaudeEffort) {
      return;
    }

    setDefaultClaudeEffort(nextValue);
    void persistAppPreferences({ defaultClaudeEffort: nextValue });
  }

  useEffect(() => {
    function handleBrowserOnline() {
      // Keep reconnect decisions in sync with the same ref-backed state path
      // that all backend connection transitions use.
      const currentConnectionState = backendConnectionStateRef.current;
      const shouldRequestReconnect = currentConnectionState !== "connected";
      if (shouldRequestReconnect) {
        setBackendConnectionState(
          latestStateRevisionRef.current === null
            ? "connecting"
            : "reconnecting",
        );
      }
      setBackendConnectionIssueDetail(null);
      clearRecoveredBackendRequestError();
      if (shouldRequestReconnect) {
        requestBackendReconnectRef.current();
      }
    }

    function handleBrowserOffline() {
      setBackendConnectionState("offline");
      setBackendConnectionIssueDetail(null);
      clearRecoveredBackendRequestError();
    }

    window.addEventListener("online", handleBrowserOnline);
    window.addEventListener("offline", handleBrowserOffline);
    return () => {
      window.removeEventListener("online", handleBrowserOnline);
      window.removeEventListener("offline", handleBrowserOffline);
    };
  }, [clearRecoveredBackendRequestError, setBackendConnectionState]);

  useEffect(() => {
    setSelectedProjectId((current) => {
      if (current === ALL_PROJECTS_FILTER_ID) {
        return current;
      }

      if (projects.some((project) => project.id === current)) {
        return current;
      }

      if (
        activeSession?.projectId &&
        projects.some((project) => project.id === activeSession.projectId)
      ) {
        return activeSession.projectId;
      }

      return projects[0]?.id ?? ALL_PROJECTS_FILTER_ID;
    });
  }, [activeSession?.projectId, projects]);

  useEffect(() => {
    const openStandaloneControlSurfaceTabIds = new Set(
      workspace.panes.flatMap((pane) =>
        pane.tabs
          .filter(
            (tab) =>
              CONTROL_SURFACE_KINDS.has(tab.kind) &&
              tab.kind !== "controlPanel",
          )
          .map((tab) => tab.id),
      ),
    );
    setStandaloneControlSurfaceViewStateByTabId((current) => {
      let changed = false;
      const next: Record<string, StandaloneControlSurfaceViewState> = {};
      for (const [tabId, state] of Object.entries(current)) {
        if (openStandaloneControlSurfaceTabIds.has(tabId)) {
          next[tabId] = state;
        } else {
          changed = true;
        }
      }
      return changed ? next : current;
    });
  }, [workspace.panes]);

  useEffect(() => {
    const validSurfaceIds = new Set(
      workspace.panes.flatMap((pane) => {
        const surfaceIds = [pane.id];
        for (const tab of pane.tabs) {
          const sectionId = resolveControlSurfaceSectionIdForWorkspaceTab(tab);
          if (sectionId) {
            surfaceIds.push(`${pane.id}-${sectionId}`);
          }
        }
        return surfaceIds;
      }),
    );
    setCollapsedSessionOrchestratorIdsBySurfaceId((current) => {
      let changed = false;
      const next: Record<string, string[]> = {};
      for (const [surfaceId, orchestratorIds] of Object.entries(current)) {
        if (validSurfaceIds.has(surfaceId)) {
          next[surfaceId] = orchestratorIds;
        } else {
          changed = true;
        }
      }
      return changed ? next : current;
    });
  }, [workspace.panes]);

  useEffect(() => {
    if (activeSession) {
      setNewSessionAgent(activeSession.agent);
    }
  }, [activeSession?.id]);

  useEffect(() => {
    if (!isWorkspaceSwitcherOpen) {
      return;
    }

    void refreshWorkspaceSummaries();
  }, [isWorkspaceSwitcherOpen, refreshWorkspaceSummaries]);

  useEffect(() => {
    if (!isWorkspaceSwitcherOpen) {
      return;
    }

    function handlePointerDown(event: PointerEvent) {
      const target = event.target;
      if (!(target instanceof Node)) {
        return;
      }

      if (workspaceSwitcherRef.current?.contains(target)) {
        return;
      }

      setIsWorkspaceSwitcherOpen(false);
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setIsWorkspaceSwitcherOpen(false);
      }
    }

    document.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);

    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [isWorkspaceSwitcherOpen]);

  useEffect(() => {
    sessionsRef.current = sessions;
  }, [sessions]);

  useEffect(() => {
    workspaceRef.current = workspace;
  }, [workspace]);

  const terminalTabIds = useMemo(
    () =>
      workspace.panes.flatMap((pane) =>
        pane.tabs.flatMap((tab) => (tab.kind === "terminal" ? [tab.id] : [])),
      ),
    [workspace.panes],
  );
  // Sort before joining so that tab drag-drop reorders do not re-fire the
  // prune effect: the effect cares about membership, not order.
  const terminalTabIdsKey = useMemo(
    () => terminalTabIds.slice().sort().join("\0"),
    [terminalTabIds],
  );

  // `terminalTabIds` is intentionally read from closure without being listed
  // in the dep array: the membership-based `terminalTabIdsKey` is the
  // source of truth for when this effect should re-run, and listing both
  // would fire it on every reorder even though nothing about membership
  // changed.
  useEffect(() => {
    pruneTerminalPanelHistory(terminalTabIds);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [terminalTabIdsKey]);

  useEffect(() => {
    const activeGitDiffRequestKeys = new Set(
      workspace.panes.flatMap((pane) =>
        pane.tabs.flatMap((tab) =>
          tab.kind === "diffPreview" && tab.gitDiffRequestKey
            ? [tab.gitDiffRequestKey]
            : [],
        ),
      ),
    );

    // Keep refresh versions monotonic for the browser process lifetime. A
    // closed diff tab can be reopened with the same request key before an old
    // fetch resolves; deleting the version here would let that stale response
    // look current.
    for (const requestKey of Array.from(
      pendingGitDiffDocumentContentRestoreKeysRef.current,
    )) {
      if (!activeGitDiffRequestKeys.has(requestKey)) {
        pendingGitDiffDocumentContentRestoreKeysRef.current.delete(requestKey);
      }
    }
    for (const requestKey of Array.from(
      attemptedGitDiffDocumentContentRestoreKeysRef.current,
    )) {
      if (!activeGitDiffRequestKeys.has(requestKey)) {
        attemptedGitDiffDocumentContentRestoreKeysRef.current.delete(
          requestKey,
        );
      }
    }
  }, [workspace.panes]);

  useEffect(() => {
    codexStateRef.current = codexState;
    agentReadinessRef.current = agentReadiness;
    projectsRef.current = projects;
    orchestratorsRef.current = orchestrators;
  }, [agentReadiness, codexState, orchestrators, projects]);

  useEffect(() => {
    draftsRef.current = draftsBySessionId;
    draftAttachmentsRef.current = draftAttachmentsBySessionId;
  }, [draftAttachmentsBySessionId, draftsBySessionId]);

  // Re-fetch Git diff preview tabs restored from persisted workspace layout
  // without `documentContent`. Layout hydration can arrive after mount, so this
  // scans every ready pane-tree change; pending/attempted request-key sets
  // dedupe actual fetches while still catching late restored tabs.
  useEffect(() => {
    if (!isWorkspaceLayoutReady) {
      return;
    }

    const restores = collectRestoredGitDiffDocumentContentRefreshes(
      workspace,
      pendingGitDiffDocumentContentRestoreKeysRef.current,
      attemptedGitDiffDocumentContentRestoreKeysRef.current,
    );
    if (restores.length === 0) {
      return;
    }

    for (const restore of restores) {
      pendingGitDiffDocumentContentRestoreKeysRef.current.add(
        restore.requestKey,
      );
      attemptedGitDiffDocumentContentRestoreKeysRef.current.add(
        restore.requestKey,
      );
      const currentVersion =
        (gitDiffPreviewRefreshVersionsRef.current.get(restore.requestKey) ??
          0) + 1;
      gitDiffPreviewRefreshVersionsRef.current.set(
        restore.requestKey,
        currentVersion,
      );

      setWorkspace((current) =>
        applyControlPanelLayout(
          updateGitDiffPreviewTabInWorkspaceState(
            current,
            restore.requestKey,
            (tab) => ({
              ...tab,
              isLoading: true,
              loadError: null,
            }),
          ),
        ),
      );

      void fetchGitDiff(restore.request)
        .then((diffPreview) => {
          pendingGitDiffDocumentContentRestoreKeysRef.current.delete(
            restore.requestKey,
          );
          if (
            !isMountedRef.current ||
            gitDiffPreviewRefreshVersionsRef.current.get(restore.requestKey) !==
              currentVersion
          ) {
            return;
          }
          appTestHooks?.onRestoredGitDiffDocumentContentUpdate?.("success");
          setWorkspace((current) =>
            applyControlPanelLayout(
              updateGitDiffPreviewTabInWorkspaceState(
                current,
                restore.requestKey,
                (tab) => ({
                  ...tab,
                  changeType: diffPreview.changeType,
                  changeSetId: diffPreview.changeSetId ?? null,
                  diff: diffPreview.diff,
                  documentEnrichmentNote:
                    diffPreview.documentEnrichmentNote ?? null,
                  documentContent: diffPreview.documentContent ?? null,
                  filePath: diffPreview.filePath ?? tab.filePath,
                  gitSectionId: restore.sectionId,
                  language: diffPreview.language ?? null,
                  summary: diffPreview.summary,
                  isLoading: false,
                  loadError: null,
                }),
              ),
            ),
          );
        })
        .catch((error) => {
          pendingGitDiffDocumentContentRestoreKeysRef.current.delete(
            restore.requestKey,
          );
          if (
            !isMountedRef.current ||
            gitDiffPreviewRefreshVersionsRef.current.get(restore.requestKey) !==
              currentVersion
          ) {
            return;
          }
          // Leave the tab visible but note the failure so the user can
          // reopen it manually. We deliberately do not close the tab: the
          // persisted stub still has a valid filePath + diff for raw
          // inspection.
          const errorMessage = getErrorMessage(error);
          appTestHooks?.onRestoredGitDiffDocumentContentUpdate?.("error");
          setWorkspace((current) =>
            applyControlPanelLayout(
              updateGitDiffPreviewTabInWorkspaceState(
                current,
                restore.requestKey,
                (tab) => ({
                  ...tab,
                  isLoading: false,
                  loadError: errorMessage,
                }),
              ),
            ),
          );
        });
    }
  }, [isWorkspaceLayoutReady, workspace.panes, workspaceViewId]);

  useEffect(() => {
    if (!workspaceFilesChangedEvent) {
      return;
    }

    const refreshes = collectGitDiffPreviewRefreshes(
      workspaceRef.current,
      workspaceFilesChangedEvent,
    );
    if (refreshes.length === 0) {
      return;
    }

    for (const refresh of refreshes) {
      const currentVersion =
        (gitDiffPreviewRefreshVersionsRef.current.get(refresh.requestKey) ??
          0) + 1;
      gitDiffPreviewRefreshVersionsRef.current.set(
        refresh.requestKey,
        currentVersion,
      );

      void fetchGitDiff(refresh.request)
        .then((diffPreview) => {
          if (
            gitDiffPreviewRefreshVersionsRef.current.get(refresh.requestKey) !==
            currentVersion
          ) {
            return;
          }

          setWorkspace((current) =>
            applyControlPanelLayout(
              updateGitDiffPreviewTabInWorkspaceState(
                current,
                refresh.requestKey,
                (tab) => ({
                  ...tab,
                  changeType: diffPreview.changeType,
                  changeSetId: diffPreview.changeSetId ?? null,
                  diff: diffPreview.diff,
                  documentEnrichmentNote:
                    diffPreview.documentEnrichmentNote ?? null,
                  documentContent: diffPreview.documentContent ?? null,
                  filePath: diffPreview.filePath ?? tab.filePath,
                  gitSectionId: refresh.sectionId,
                  language: diffPreview.language ?? null,
                  summary: diffPreview.summary,
                  isLoading: false,
                  loadError: null,
                }),
              ),
            ),
          );
        })
        .catch((error) => {
          if (
            gitDiffPreviewRefreshVersionsRef.current.get(refresh.requestKey) !==
            currentVersion
          ) {
            return;
          }

          const errorMessage = getErrorMessage(error);
          setWorkspace((current) =>
            applyControlPanelLayout(
              updateGitDiffPreviewTabInWorkspaceState(
                current,
                refresh.requestKey,
                (tab) => ({
                  ...tab,
                  diff: "",
                  documentEnrichmentNote: null,
                  documentContent: null,
                  summary: `Failed to refresh ${refresh.sectionId} changes in ${refresh.request.path}`,
                  isLoading: false,
                  loadError: errorMessage,
                }),
              ),
            ),
          );
        });
    }
  }, [workspaceFilesChangedEvent]);

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
      activePromptPollCancelRef.current?.();
      activePromptPollCancelRef.current = null;
    };
  }, []);

  useEffect(() => {
    return () => {
      releaseDraftAttachments(
        Object.values(draftAttachmentsRef.current).flat(),
      );
    };
  }, []);

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      const key = event.key.toLowerCase();
      const hasPrimaryModifier = event.metaKey || event.ctrlKey;
      if (
        event.defaultPrevented ||
        key !== "f" ||
        !hasPrimaryModifier ||
        !event.shiftKey ||
        event.altKey ||
        isSettingsOpen ||
        pendingKillSessionId
      ) {
        return;
      }

      event.preventDefault();
      focusSessionListSearch(true);
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [isSettingsOpen, pendingKillSessionId]);

  function renderWorkspaceControlSurface(
    paneId: string,
    fixedSection: ControlPanelSectionId | null = null,
  ): JSX.Element {
    return (
      <AppControlSurface
        paneId={paneId}
        fixedSection={fixedSection}
        controlPanelSurfaceRef={controlPanelSurfaceRef}
        collapsedSessionOrchestratorIdsBySurfaceId={
          collapsedSessionOrchestratorIdsBySurfaceId
        }
        paneLookup={paneLookup}
        sessionLookup={sessionLookup}
        workspace={workspace}
        standaloneControlSurfaceViewStateByTabId={
          standaloneControlSurfaceViewStateByTabId
        }
        projectLookup={projectLookup}
        selectedProjectId={selectedProjectId}
        activeSession={activeSession}
        sessions={sessions}
        orchestrators={orchestrators}
        openSessionIds={openSessionIds}
        sessionListFilter={sessionListFilter}
        setSessionListFilter={setSessionListFilter}
        sessionListSearchQuery={sessionListSearchQuery}
        setSessionListSearchQuery={setSessionListSearchQuery}
        sessionFilterCounts={sessionFilterCounts}
        hasSessionListSearch={hasSessionListSearch}
        sessionListSearchResults={sessionListSearchResults}
        filteredSessions={filteredSessions}
        controlPanelFilesystemRoot={controlPanelFilesystemRoot}
        controlPanelGitWorkdir={controlPanelGitWorkdir}
        controlPanelGitStatusCount={controlPanelGitStatusCount}
        setControlPanelGitStatusCount={setControlPanelGitStatusCount}
        workspaceFilesChangedEvent={workspaceFilesChangedEvent}
        projects={projects}
        projectSessionCounts={projectSessionCounts}
        remoteLookup={remoteLookup}
        projectScopedSessions={projectScopedSessions}
        isSettingsOpen={isSettingsOpen}
        setIsSettingsOpen={setIsSettingsOpen}
        isCreateSessionOpen={isCreateSessionOpen}
        isCreating={isCreating}
        isCreatingProject={isCreatingProject}
        controlPanelProjectOptions={controlPanelProjectOptions}
        controlPanelInlineIssueDetail={controlPanelInlineIssueDetail}
        backendConnectionState={backendConnectionState}
        workspaceViewId={workspaceViewId}
        deletingWorkspaceIds={deletingWorkspaceIds}
        workspaceSwitcherError={workspaceSwitcherError}
        isWorkspaceSwitcherLoading={isWorkspaceSwitcherLoading}
        isWorkspaceSwitcherOpen={isWorkspaceSwitcherOpen}
        workspaceSummaries={workspaceSummaries}
        workspaceSwitcherRef={workspaceSwitcherRef}
        windowId={windowId}
        pendingOrchestratorActionById={pendingOrchestratorActionById}
        killingSessionIds={killingSessionIds}
        pendingKillSessionId={pendingKillSessionId}
        killRevealSessionId={killRevealSessionId}
        sessionListSearchInputRef={sessionListSearchInputRef}
        setKillRevealSessionId={setKillRevealSessionId}
        setControlPanelFilesystemRoot={setControlPanelFilesystemRoot}
        setControlPanelGitWorkdir={setControlPanelGitWorkdir}
        setStandaloneControlSurfaceViewStateByTabId={
          setStandaloneControlSurfaceViewStateByTabId
        }
        setCollapsedSessionOrchestratorIdsBySurfaceId={
          setCollapsedSessionOrchestratorIdsBySurfaceId
        }
        setSelectedProjectId={setSelectedProjectId}
        setWorkspace={setWorkspace}
        handleSidebarSessionClick={handleSidebarSessionClick}
        handleKillSession={handleKillSession}
        handleSessionRenameRequest={handleSessionRenameRequest}
        handleControlPanelLauncherDragStart={
          handleControlPanelLauncherDragStart
        }
        handleControlPanelLauncherDragEnd={handleControlPanelLauncherDragEnd}
        handleOpenFilesystemTab={handleOpenFilesystemTab}
        handleOpenGitStatusTab={handleOpenGitStatusTab}
        handleOpenGitStatusDiffPreviewTab={handleOpenGitStatusDiffPreviewTab}
        handleOpenProjectListTab={handleOpenProjectListTab}
        handleOpenOrchestratorListTab={handleOpenOrchestratorListTab}
        handleOpenOrchestratorCanvasTab={handleOpenOrchestratorCanvasTab}
        handleOpenSessionListTab={handleOpenSessionListTab}
        handleOpenCanvasTab={handleOpenCanvasTab}
        openCreateProjectDialog={openCreateProjectDialog}
        openCreateSessionDialog={openCreateSessionDialog}
        handleOpenSourceTab={handleOpenSourceTab}
        handleOrchestratorStateUpdated={handleOrchestratorStateUpdated}
        handleProjectMenuRemoveProject={handleProjectMenuRemoveProject}
        handleProjectMenuStartSession={handleProjectMenuStartSession}
        handleOrchestratorRuntimeAction={handleOrchestratorRuntimeAction}
        handleDeleteWorkspace={handleDeleteWorkspace}
        handleOpenNewWorkspaceHere={handleOpenNewWorkspaceHere}
        handleOpenNewWorkspaceWindow={handleOpenNewWorkspaceWindow}
        handleOpenWorkspaceHere={handleOpenWorkspaceHere}
        handleWorkspaceSwitcherToggle={handleWorkspaceSwitcherToggle}
        handleRetryBackendConnection={handleRetryBackendConnection}
      />
    );
  }

  function renderControlPanelPaneBarActions(): JSX.Element {
    return (
      <WorkspaceSwitcher
        currentWorkspaceId={workspaceViewId}
        deletingWorkspaceIds={deletingWorkspaceIds}
        error={workspaceSwitcherError}
        isLoading={isWorkspaceSwitcherLoading}
        isOpen={isWorkspaceSwitcherOpen}
        summaries={workspaceSummaries}
        switcherRef={workspaceSwitcherRef}
        onDeleteWorkspace={handleDeleteWorkspace}
        onOpenNewWorkspaceHere={handleOpenNewWorkspaceHere}
        onOpenNewWorkspaceWindow={handleOpenNewWorkspaceWindow}
        onOpenWorkspace={handleOpenWorkspaceHere}
        onToggle={handleWorkspaceSwitcherToggle}
      />
    );
  }

  function renderControlPanelPaneBarStatus(): JSX.Element | null {
    return (
      <ControlPanelConnectionIndicator
        state={backendConnectionState}
        issueDetail={controlPanelInlineIssueDetail}
        onRetry={
          backendConnectionState === "connecting" ||
          backendConnectionState === "reconnecting"
            ? handleRetryBackendConnection
            : undefined
        }
      />
    );
  }

  return (
    <div className="shell">
      <div className="background-orbit background-orbit-left" />
      <div className="background-orbit background-orbit-right" />

      <main className="workspace-shell">
        {requestError && !requestErrorShownInline ? (
          <article className="thread-notice workspace-notice">
            <div className="card-label">Backend</div>
            <p>{requestError}</p>
          </article>
        ) : null}

        <section
          className={`workspace-stage${
            workspaceHasOnlyControlPanel
              ? ` workspace-stage-control-panel-only workspace-stage-control-panel-only-${controlPanelSide}`
              : ""
          }`}
        >
          {workspace.root ? (
            <WorkspaceNodeView
              node={workspace.root}
              codexState={codexState}
              projectLookup={projectLookup}
              remoteLookup={remoteLookup}
              paneLookup={paneLookup}
              sessionLookup={sessionLookup}
              activePaneId={workspace.activePaneId}
              isLoading={isLoading}
              sendingSessionIds={sendingSessionIds}
              stoppingSessionIds={stoppingSessionIds}
              killingSessionIds={killingSessionIds}
              updatingSessionIds={updatingSessionIds}
              refreshingSessionModelOptionIds={refreshingSessionModelOptionIds}
              sessionModelOptionErrors={sessionModelOptionErrors}
              agentCommandsBySessionId={agentCommandsBySessionId}
              refreshingAgentCommandSessionIds={
                refreshingAgentCommandSessionIds
              }
              agentCommandErrors={agentCommandErrors}
              sessionSettingNotices={sessionSettingNotices}
              paneShouldStickToBottomRef={paneShouldStickToBottomRef}
              paneScrollPositionsRef={paneScrollPositionsRef}
              paneContentSignaturesRef={paneContentSignaturesRef}
              forceSessionScrollToBottomRef={forceSessionScrollToBottomRef}
              pendingScrollToBottomRequest={pendingScrollToBottomRequest}
              windowId={windowId}
              draggedTab={activeDraggedTab}
              getKnownDraggedTab={getKnownWorkspaceTabDrag}
              editorAppearance={editorAppearance}
              editorFontSizePx={editorFontSizePx}
              onActivatePane={handlePaneActivate}
              onSelectTab={handlePaneTabSelect}
              onCloseTab={handleCloseTab}
              onSplitPane={handleSplitPane}
              onResizeStart={handleSplitResizeStart}
              onTabDragStart={handleTabDragStart}
              onTabDragEnd={handleTabDragEnd}
              onTabDrop={handleTabDrop}
              onPaneViewModeChange={handlePaneViewModeChange}
              onOpenSourceTab={handleOpenSourceTab}
              onOpenDiffPreviewTab={handleOpenDiffPreviewTab}
              onOpenGitStatusDiffPreviewTab={handleOpenGitStatusDiffPreviewTab}
              onOpenFilesystemTab={handleOpenFilesystemTab}
              onOpenGitStatusTab={handleOpenGitStatusTab}
              onOpenTerminalTab={handleOpenTerminalTab}
              onOpenInstructionDebuggerTab={handleOpenInstructionDebuggerTab}
              onOpenCanvasTab={handleOpenCanvasTab}
              onUpsertCanvasSessionCard={handleUpsertCanvasSessionCard}
              onRemoveCanvasSessionCard={handleRemoveCanvasSessionCard}
              onSetCanvasZoom={handleSetCanvasZoom}
              onPaneSourcePathChange={handlePaneSourcePathChange}
              onOpenConversationFromDiff={handleOpenConversationFromDiff}
              onInsertReviewIntoPrompt={handleInsertReviewIntoPrompt}
              onDraftCommit={handleDraftChange}
              onDraftAttachmentsAdd={handleDraftAttachmentsAdd}
              onDraftAttachmentRemove={handleDraftAttachmentRemove}
              onComposerError={setRequestError}
              onSend={handleSend}
              onCancelQueuedPrompt={handleCancelQueuedPrompt}
              onApprovalDecision={handleApprovalDecision}
              onUserInputSubmit={handleUserInputSubmit}
              onMcpElicitationSubmit={handleMcpElicitationSubmit}
              onCodexAppRequestSubmit={handleCodexAppRequestSubmit}
              onStopSession={handleStopSession}
              onKillSession={handleKillSession}
              onRenameSessionRequest={handleSessionRenameRequest}
              onScrollToBottomRequestHandled={
                handleScrollToBottomRequestHandled
              }
              onSessionSettingsChange={handleSessionSettingsChange}
              onArchiveCodexThread={handleArchiveCodexThread}
              onCompactCodexThread={handleCompactCodexThread}
              onForkCodexThread={handleForkCodexThread}
              onRefreshSessionModelOptions={handleRefreshSessionModelOptions}
              onRefreshAgentCommands={handleRefreshAgentCommands}
              onRollbackCodexThread={handleRollbackCodexThread}
              onUnarchiveCodexThread={handleUnarchiveCodexThread}
              onOrchestratorStateUpdated={handleOrchestratorStateUpdated}
              renderControlPanel={renderWorkspaceControlSurface}
              renderControlPanelPaneBarStatus={renderControlPanelPaneBarStatus}
              renderControlPanelPaneBarActions={
                renderControlPanelPaneBarActions
              }
              workspaceFilesChangedEvent={workspaceFilesChangedEvent}
              backendConnectionState={backendConnectionState}
            />
          ) : (
            <div className="workspace-empty panel">
              <EmptyState
                title={
                  isLoading
                    ? "Connecting to backend"
                    : "No sessions in the workspace"
                }
                body={
                  isLoading
                    ? "Fetching session state from the Rust backend."
                    : "Select a session from the left rail or create a new one to start tiling."
                }
              />
            </div>
          )}
        </section>
      </main>
      <AppDialogs
        pendingKillSession={pendingKillSession}
        pendingKillPopoverRef={pendingKillPopoverRef}
        pendingKillPopoverStyle={pendingKillPopoverStyle}
        pendingKillConfirmButtonRef={pendingKillConfirmButtonRef}
        schedulePendingKillConfirmationClose={schedulePendingKillConfirmationClose}
        clearPendingKillCloseTimeout={clearPendingKillCloseTimeout}
        closePendingKillConfirmation={closePendingKillConfirmation}
        confirmKillSession={confirmKillSession}
        pendingSessionRenameSession={pendingSessionRenameSession}
        pendingSessionRenamePopoverRef={pendingSessionRenamePopoverRef}
        pendingSessionRenameStyle={pendingSessionRenameStyle}
        pendingSessionRenameInputRef={pendingSessionRenameInputRef}
        pendingSessionRenameDraft={pendingSessionRenameDraft}
        pendingSessionRenameValue={pendingSessionRenameValue}
        isPendingSessionRenameCreating={isPendingSessionRenameCreating}
        isPendingSessionRenameSubmitting={isPendingSessionRenameSubmitting}
        isPendingSessionRenameKilling={isPendingSessionRenameKilling}
        schedulePendingSessionRenameClose={schedulePendingSessionRenameClose}
        clearPendingSessionRenameCloseTimeout={clearPendingSessionRenameCloseTimeout}
        closePendingSessionRename={closePendingSessionRename}
        confirmSessionRename={confirmSessionRename}
        setPendingSessionRenameDraft={setPendingSessionRenameDraft}
        handlePendingSessionRenameNew={handlePendingSessionRenameNew}
        handlePendingSessionRenameKill={handlePendingSessionRenameKill}
        requestError={requestError}
        isCreateSessionOpen={isCreateSessionOpen}
        isCreating={isCreating}
        closeCreateSessionDialog={() => {
          setRequestError(null);
          setIsCreateSessionOpen(false);
        }}
        handleCreateSessionDialogSubmit={handleCreateSessionDialogSubmit}
        newSessionAgent={newSessionAgent}
        onChangeNewSessionAgent={setNewSessionAgent}
        createSessionUsesSessionModelPicker={createSessionUsesSessionModelPicker}
        newSessionModel={newSessionModel}
        newSessionModelOptions={newSessionModelOptions}
        onChangeNewSessionModel={(nextValue) =>
          setNewSessionModelByAgent((current) => ({
            ...current,
            [newSessionAgent]: nextValue,
          }))
        }
        defaultCodexReasoningEffort={defaultCodexReasoningEffort}
        handleDefaultCodexReasoningEffortChange={handleDefaultCodexReasoningEffortChange}
        defaultClaudeEffort={defaultClaudeEffort}
        handleDefaultClaudeEffortChange={handleDefaultClaudeEffortChange}
        defaultCursorMode={defaultCursorMode}
        onChangeDefaultCursorMode={setDefaultCursorMode}
        defaultGeminiApprovalMode={defaultGeminiApprovalMode}
        onChangeDefaultGeminiApprovalMode={setDefaultGeminiApprovalMode}
        createSessionProjectId={createSessionProjectId}
        createSessionProjectOptions={createSessionProjectOptions}
        onChangeCreateSessionProjectId={setCreateSessionProjectId}
        createSessionProjectHint={createSessionProjectHint}
        createSessionProjectSelectionError={createSessionProjectSelectionError}
        createSessionAgentReadiness={createSessionAgentReadiness}
        createSessionBlocked={createSessionBlocked}
        isCreateProjectOpen={isCreateProjectOpen}
        isCreatingProject={isCreatingProject}
        closeCreateProjectDialog={() => {
          setRequestError(null);
          setIsCreateProjectOpen(false);
        }}
        handleCreateProject={handleCreateProject}
        newProjectRemoteId={newProjectRemoteId}
        createProjectRemoteOptions={createProjectRemoteOptions}
        onChangeNewProjectRemoteId={setNewProjectRemoteId}
        newProjectSelectedRemote={newProjectSelectedRemote}
        newProjectUsesLocalRemote={newProjectUsesLocalRemote}
        newProjectRootPath={newProjectRootPath}
        onChangeNewProjectRootPath={setNewProjectRootPath}
        handlePickProjectRoot={handlePickProjectRoot}
        isSettingsOpen={isSettingsOpen}
        closeSettingsDialog={() => setIsSettingsOpen(false)}
        settingsTab={settingsTab}
        setSettingsTab={setSettingsTab}
        activeStyle={activeStyle}
        activeTheme={activeTheme}
        styleId={styleId}
        themeId={themeId}
        setStyleId={setStyleId}
        setThemeId={setThemeId}
        activeMarkdownTheme={activeMarkdownTheme}
        activeMarkdownStyle={activeMarkdownStyle}
        markdownThemeId={markdownThemeId}
        markdownStyleId={markdownStyleId}
        diagramThemeOverrideMode={diagramThemeOverrideMode}
        diagramLook={diagramLook}
        diagramPalette={diagramPalette}
        setMarkdownThemeId={setMarkdownThemeId}
        setMarkdownStyleId={setMarkdownStyleId}
        setDiagramThemeOverrideMode={setDiagramThemeOverrideMode}
        setDiagramLook={setDiagramLook}
        setDiagramPalette={setDiagramPalette}
        densityPercent={densityPercent}
        editorFontSizePx={editorFontSizePx}
        fontSizePx={fontSizePx}
        setDensityPercent={setDensityPercent}
        setEditorFontSizePx={setEditorFontSizePx}
        setFontSizePx={setFontSizePx}
        remoteConfigs={remoteConfigs}
        onSaveRemotes={(nextRemotes) => {
          void persistAppPreferences({ remotes: nextRemotes });
        }}
        projects={projects}
        sessions={sessions}
        handleOrchestratorStateUpdated={handleOrchestratorStateUpdated}
        defaultCodexApprovalPolicy={defaultCodexApprovalPolicy}
        defaultCodexSandboxMode={defaultCodexSandboxMode}
        setDefaultCodexApprovalPolicy={setDefaultCodexApprovalPolicy}
        setDefaultCodexSandboxMode={setDefaultCodexSandboxMode}
        defaultClaudeApprovalMode={defaultClaudeApprovalMode}
        setDefaultClaudeApprovalMode={handleDefaultClaudeApprovalModeChange}
      />
    </div>
  );
}
