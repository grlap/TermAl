import {
  startTransition,
  useCallback,
  useDeferredValue,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ClipboardEvent as ReactClipboardEvent,
  type DragEvent as ReactDragEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  type RefObject,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { createPortal, flushSync } from "react-dom";
import {
  CommandCard,
  DialogCloseIcon,
  DiffCard,
  MessageCard,
  MarkdownContent,
  type MarkdownFileLinkTarget,
} from "./message-cards";
import {
  archiveCodexThread,
  cancelQueuedPrompt,
  compactCodexThread,
  createProject,
  createSession,
  deleteWorkspaceLayout,
  fetchAgentCommands,
  fetchFile,
  fetchGitDiff,
  fetchGitStatus,
  fetchState,
  fetchWorkspaceLayout,
  fetchWorkspaceLayouts,
  forkCodexThread,
  isBackendUnavailableError,
  killSession,
  pauseOrchestratorInstance,
  pickProjectRoot,
  refreshSessionModelOptions,
  resumeOrchestratorInstance,
  stopOrchestratorInstance,
  renameSession,
  rollbackCodexThread,
  saveFile,
  saveWorkspaceLayout,
  sendMessage,
  stopSession,
  submitApproval,
  submitCodexAppRequest,
  submitMcpElicitation,
  submitUserInput,
  type FileResponse,
  type GitDiffRequestPayload,
  type GitDiffSection,
  type StateResponse,
  type WorkspaceLayoutSummary,
  unarchiveCodexThread,
  updateAppSettings,
  updateSessionSettings,
} from "./api";
import { AgentIcon } from "./agent-icon";
import {
  LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS,
  LIVE_SESSION_WATCHDOG_RESYNC_RETRY_COOLDOWN_MS,
  applyDeltaToSessions,
  pruneLiveTransportActivitySessions,
  sessionHasPotentiallyStaleTransport,
} from "./live-updates";
import {
  areRemoteConfigsEqual,
  createSessionModelHint,
  DEFAULT_CLAUDE_EFFORT,
  DEFAULT_CODEX_REASONING_EFFORT,
  defaultNewSessionModel,
  describeCodexModelAdjustmentNotice,
  describeProjectScope,
  describeSessionModelRefreshError,
  describeUnknownSessionModelWarning,
  normalizedCodexReasoningEffort,
  normalizedRequestedSessionModel,
  resolveAppPreferences,
  resolveControlPanelWorkspaceRoot,
  resolveRemoteConfig,
  resolveUnknownSessionModelSendAttempt,
  remoteBadgeLabel,
  unknownSessionModelConfirmationKey,
  usesSessionModelPicker,
  CLAUDE_EFFORT_OPTIONS,
  CODEX_REASONING_EFFORT_OPTIONS,
  NEW_SESSION_MODEL_OPTIONS,
  type ComboboxOption,
} from "./session-model-utils";

// Re-export public API used by test files
export {
  describeCodexModelAdjustmentNotice,
  describeSessionModelRefreshError,
  describeUnknownSessionModelWarning,
  resolveControlPanelWorkspaceRoot,
  resolveUnknownSessionModelSendAttempt,
  type ComboboxOption,
} from "./session-model-utils";

export { MessageCard, MarkdownContent } from "./message-cards";

import {
  ThemePreferencesPanel,
  AppearancePreferencesPanel,
  RemotePreferencesPanel,
  ClaudeApprovalsPreferencesPanel,
  CodexPromptPreferencesPanel,
  ThemedCombobox,
  CURSOR_MODE_OPTIONS,
  GEMINI_APPROVAL_OPTIONS,
} from "./preferences-panels";

import {
  CodexPromptSettingsCard,
  ClaudePromptSettingsCard,
  CursorPromptSettingsCard,
  GeminiPromptSettingsCard,
} from "./prompt-settings-cards";

export { ThemedCombobox } from "./preferences-panels";
export {
  CodexPromptSettingsCard,
  ClaudePromptSettingsCard,
  CursorPromptSettingsCard,
  GeminiPromptSettingsCard,
} from "./prompt-settings-cards";

import { normalizeDisplayPath } from "./path-display";
import {
  LOCAL_REMOTE_ID,
  createBuiltinLocalRemote,
  isLocalRemoteId,
  remoteConnectionLabel,
  remoteDisplayName,
  resolveProjectRemoteId,
} from "./remotes";
import { resolvePaneScrollCommand } from "./pane-keyboard";
import {
  ControlPanelConnectionIndicator,
  WorkspaceSwitcher,
} from "./workspace-shell-controls";
import {
  RuntimeActionButton,
  type RuntimeAction,
} from "./runtime-action-button";
import {
  AgentSessionPanel,
  AgentSessionPanelFooter,
} from "./panels/AgentSessionPanel";
import {
  ControlPanelSectionIcon,
  ControlPanelSurface,
  type ControlPanelSectionId,
  type ControlPanelSurfaceHandle,
} from "./panels/ControlPanelSurface";
import { DiffPanel } from "./panels/DiffPanel";
import { FileSystemPanel } from "./panels/FileSystemPanel";
import { GitStatusPanel } from "./panels/GitStatusPanel";
import { InstructionDebuggerPanel } from "./panels/InstructionDebuggerPanel";
import { OrchestratorTemplateLibraryPanel } from "./panels/OrchestratorTemplateLibraryPanel";
import { PaneTabs, type PaneTabDecoration } from "./panels/PaneTabs";
import { OrchestratorTemplatesPanel } from "./panels/OrchestratorTemplatesPanel";
import { SessionCanvasPanel } from "./panels/SessionCanvasPanel";
import {
  SourcePanel,
  type SourceFileState,
  type SourceSaveOptions,
} from "./panels/SourcePanel";
import {
  buildSessionListSearchResultFromIndex,
  buildSessionSearchIndex,
  buildSessionSearchMatchesFromIndex,
  type SessionListSearchResult,
  type SessionSearchMatch,
} from "./session-find";
import type {
  AppPreferences,
  ApprovalDecision,
  ApprovalPolicy,
  AgentCommand,
  AgentReadiness,
  AgentType,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CommandMessage,
  CodexReasoningEffort,
  CodexState,
  CursorMode,
  DeltaEvent,
  DiffMessage,
  ExhaustiveValueCoverage,
  GeminiApprovalMode,
  JsonValue,
  Message,
  McpElicitationAction,
  PendingPrompt,
  OrchestratorInstance,
  Project,
  RemoteConfig,
  SandboxMode,
  Session,
  SessionModelOption,
  SessionSettingsField,
  SessionSettingsValue,
  WorkspaceFilesChangedEvent,
} from "./types";
import {
  activatePane,
  closeWorkspaceTab,
  CONTROL_SURFACE_KINDS,
  createFilesystemTab,
  createGitStatusTab,
  createOrchestratorListTab,
  createProjectListTab,
  createSessionListTab,
  DEFAULT_CONTROL_PANEL_DOCK_WIDTH_RATIO,
  dockControlPanelAtWorkspaceEdge,
  ensureControlPanelInWorkspaceState,
  findNearestControlSurfacePaneId,
  findNearestSessionPaneId,
  findWorkspacePaneIdForSession,
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
  stripLoadingGitDiffPreviewTabsFromWorkspaceState,
  updateGitDiffPreviewTabInWorkspaceState,
  updateSplitRatio,
  upsertCanvasSessionCard,
  type PaneViewMode,
  type SessionPaneViewMode,
  type TabDropPlacement,
  type WorkspaceNode,
  type WorkspacePane,
  type WorkspaceState,
  type WorkspaceTab,
} from "./workspace";
import {
  createWorkspaceViewId,
  deleteStoredWorkspaceLayout,
  ensureWorkspaceViewId,
  getStoredWorkspaceLayout,
  parseStoredWorkspaceLayout,
  persistWorkspaceLayout,
  type ControlPanelSide,
  WORKSPACE_VIEW_QUERY_PARAM,
} from "./workspace-storage";
import { reconcileSessions } from "./session-reconcile";
import {
  attachSessionDragData,
  dataTransferHasSessionDragType,
  readSessionDragData,
} from "./session-drag";
import {
  DENSITY_STEP_PERCENT,
  DEFAULT_DENSITY_PERCENT,
  DEFAULT_EDITOR_FONT_SIZE_PX,
  DEFAULT_FONT_SIZE_PX,
  MAX_DENSITY_PERCENT,
  MAX_EDITOR_FONT_SIZE_PX,
  MAX_FONT_SIZE_PX,
  MIN_DENSITY_PERCENT,
  MIN_EDITOR_FONT_SIZE_PX,
  MIN_FONT_SIZE_PX,
  STYLES,
  THEMES,
  applyDensityPreference,
  applyFontSizePreference,
  applyStylePreference,
  applyThemePreference,
  clampDensityPreference,
  clampEditorFontSizePreference,
  clampFontSizePreference,
  getStoredDensityPreference,
  getStoredEditorFontSizePreference,
  getStoredFontSizePreference,
  getStoredStylePreference,
  getStoredThemePreference,
  persistDensityPreference,
  persistEditorFontSizePreference,
  persistFontSizePreference,
  persistStylePreference,
  persistThemePreference,
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
  decideDeltaRevisionAction,
  shouldAdoptSnapshotRevision,
} from "./state-revision";
import {
  TAB_DRAG_CHANNEL_NAME,
  TAB_DRAG_MIME_TYPE,
  attachWorkspaceTabDragData,
  createWorkspaceTabDrag,
  isWorkspaceTabDragChannelMessage,
  readWorkspaceTabDragData,
  type WorkspaceTabDrag,
  type WorkspaceTabDragChannelMessage,
} from "./tab-drag";
import {
  canNestedScrollableConsumeWheel,
  clamp,
  buildGitDiffPreviewRequestKey,
  buildMessageListSignature,
  buildSessionConversationSignature,
  collectCandidateSourcePaths,
  collectClipboardImageFiles,
  createDraftAttachmentsFromFiles,
  dropLabelForPlacement,
  findLastUserPrompt,
  formatByteSize,
  getErrorMessage,
  isHexColorDark,
  isPointerWithinPaneTopArea,
  labelForPaneViewMode,
  labelForStatus,
  MAX_PASTED_IMAGE_BYTES,
  messageChangeMarker,
  normalizeWheelDelta,
  pendingGitDiffPreviewChangeType,
  pendingGitDiffPreviewSummary,
  primaryModifierLabel,
  pruneSessionAttachmentValues,
  pruneSessionCommandValues,
  pruneSessionFlags,
  pruneSessionFlagsWithInvalidation,
  pruneSessionValues,
  readNavigatorOnline,
  releaseDraftAttachments,
  removeQueuedPromptFromSessions,
  resolvePaneDropPlacementFromPointer,
  setSessionFlag,
  SUPPORTED_PASTED_IMAGE_TYPES,
  type DraftImageAttachment,
  type SessionAgentCommandMap,
  type SessionFlagMap,
} from "./app-utils";
import {
  mergeWorkspaceFilesChangedEvents,
  workspaceFilesChangedEventChangeForPath,
  workspaceFilesChangedEventTouchesGitDiffTab,
} from "./workspace-file-events";

const TAB_DRAG_STALE_TIMEOUT_MS = 15000;
const RECONNECT_STATE_RESYNC_DELAY_MS = 400;
const RECONNECT_STATE_RESYNC_MAX_DELAY_MS = 5000;
const LIVE_SESSION_RESUME_WATCHDOG_INTERVAL_MS = 1000;

const WORKSPACE_LAYOUT_PERSIST_DELAY_MS = 150;
const BACKEND_UNAVAILABLE_ISSUE_DETAIL =
  "Could not reach the TermAl backend. Retrying automatically.";
const BACKEND_SYNC_ISSUE_DETAIL =
  "A live backend update could not be processed. Waiting for the next successful sync.";

function describeBackendConnectionIssueDetail(error: unknown) {
  if (isBackendUnavailableError(error)) {
    // Incompatible backend serving HTML instead of JSON — surface the restart
    // instruction directly rather than the generic connectivity message.
    return error.restartRequired
      ? error.message
      : BACKEND_UNAVAILABLE_ISSUE_DETAIL;
  }
  return BACKEND_SYNC_ISSUE_DETAIL;
}

export function resolveRecoveredWorkspaceLayoutRequestError(
  currentRequestError: string | null,
  workspaceLayoutRestartErrorMessage: string | null,
) {
  if (workspaceLayoutRestartErrorMessage === null) {
    return currentRequestError;
  }

  return currentRequestError === workspaceLayoutRestartErrorMessage
    ? null
    : currentRequestError;
}

// Re-exported from ./types for backward compatibility
export type { SessionSettingsField, SessionSettingsValue } from "./types";
type SessionErrorMap = Record<string, string | undefined>;
type StateEventPayload = StateResponse & {
  _sseFallback?: boolean;
};
type SessionNoticeMap = Record<string, string | undefined>;
type BackendConnectionState =
  | "connecting"
  | "connected"
  | "reconnecting"
  | "offline";
type WorkspaceLayoutPersistencePayload = {
  controlPanelSide: ControlPanelSide;
  densityPercent: number;
  editorFontSizePx: number;
  fontSizePx: number;
  styleId: StyleId;
  themeId: ThemeId;
  workspace: WorkspaceState;
};
type PendingWorkspaceLayoutSave = {
  layout: WorkspaceLayoutPersistencePayload;
  workspaceId: string;
};
type OrchestratorRuntimeAction = RuntimeAction;
type PreferencesTabId =
  | "themes"
  | "appearance"
  | "remotes"
  | "orchestrators"
  | "codex-prompts"
  | "claude-approvals";
type PendingSessionRename = {
  clientX: number;
  clientY: number;
  sessionId: string;
};

function sourceFileStateFromResponse(response: FileResponse): SourceFileState {
  return {
    status: "ready",
    path: response.path,
    content: response.content,
    contentHash: response.contentHash ?? null,
    mtimeMs: response.mtimeMs ?? null,
    sizeBytes: response.sizeBytes ?? null,
    staleOnDisk: false,
    externalChangeKind: null,
    externalContentHash: null,
    externalMtimeMs: null,
    externalSizeBytes: null,
    error: null,
    language: response.language ?? null,
  };
}

type GitDiffPreviewRefresh = {
  request: GitDiffRequestPayload;
  requestKey: string;
  sectionId: GitDiffSection;
};

function collectGitDiffPreviewRefreshes(
  workspace: WorkspaceState,
  event: WorkspaceFilesChangedEvent,
): GitDiffPreviewRefresh[] {
  const refreshes = new Map<string, GitDiffPreviewRefresh>();

  for (const pane of workspace.panes) {
    for (const tab of pane.tabs) {
      if (
        tab.kind !== "diffPreview" ||
        !tab.gitDiffRequestKey ||
        !tab.gitDiffRequest ||
        !workspaceFilesChangedEventTouchesGitDiffTab(event, tab)
      ) {
        continue;
      }

      refreshes.set(tab.gitDiffRequestKey, {
        request: tab.gitDiffRequest,
        requestKey: tab.gitDiffRequestKey,
        sectionId: tab.gitSectionId ?? tab.gitDiffRequest.sectionId,
      });
    }
  }

  return Array.from(refreshes.values());
}

function isSourceFileMissingError(error: unknown) {
  const message = getErrorMessage(error).toLowerCase();
  return message.includes("file not found") || message.includes("not found");
}

const PENDING_KILL_CLOSE_DELAY_MS = 180;
const PENDING_SESSION_RENAME_CLOSE_DELAY_MS = 300;
const DEFAULT_SPLIT_MIN_RATIO = 0.22;
const DEFAULT_SPLIT_MAX_RATIO = 0.78;
// 40rem is the minimum acceptable docked control-panel width. Keep these
// fallbacks aligned with the CSS dock width/min-width so saved layouts do not
// permit a narrower manual resize that later snaps back.
const CONTROL_PANEL_PANE_MIN_WIDTH_FALLBACK_PX = 40 * 16;
const STANDALONE_CONTROL_SURFACE_PANE_MIN_WIDTH_FALLBACK_PX = 16 * 16;
const CONTROL_PANEL_PANE_WIDTH_FALLBACK_PX = 40 * 16;

type SessionConversationItem =
  | {
      author: Message["author"];
      id: string;
      kind: "message";
      message: Message;
    }
  | {
      author: "you";
      id: string;
      kind: "pendingPrompt";
      prompt: PendingPrompt;
    };

const MAX_CACHED_SESSION_PAGES_PER_PANE = 3;
const NEW_SESSION_AGENT_OPTIONS = [
  { label: "Claude", value: "Claude" },
  { label: "Codex", value: "Codex" },
  { label: "Cursor", value: "Cursor" },
  { label: "Gemini", value: "Gemini" },
] as const satisfies ReadonlyArray<{ label: string; value: AgentType }>;
const NEW_SESSION_AGENT_OPTIONS_EXHAUSTIVE: ExhaustiveValueCoverage<
  AgentType,
  typeof NEW_SESSION_AGENT_OPTIONS
> = true;
const PREFERENCES_TABS: ReadonlyArray<{ id: PreferencesTabId; label: string }> =
  [
    { id: "themes", label: "Themes" },
    { id: "appearance", label: "Editor & UI appearance" },
    { id: "remotes", label: "Remotes" },
    { id: "orchestrators", label: "Orchestrators" },
    { id: "codex-prompts", label: "Codex defaults" },
    { id: "claude-approvals", label: "Claude defaults" },
  ];
const ALL_PROJECTS_FILTER_ID = "__all__";
const CREATE_SESSION_WORKSPACE_ID = "__workspace__";
type StandaloneControlSurfaceViewState = {
  projectId?: string;
  sessionListFilter?: SessionListFilter;
  sessionListSearchQuery?: string;
};

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
    case "instructionDebugger":
    case "diffPreview":
      return null;
  }
}

export function resolveAdoptedStateSlices(
  current: {
    codex: CodexState;
    agentReadiness: AgentReadiness[];
    projects: Project[];
    orchestrators: OrchestratorInstance[];
    workspaces: WorkspaceLayoutSummary[];
  },
  nextState: Partial<
    Pick<
      StateResponse,
      "codex" | "agentReadiness" | "projects" | "orchestrators" | "workspaces"
    >
  >,
) {
  return {
    codex: nextState.codex !== undefined ? nextState.codex : current.codex,
    agentReadiness:
      nextState.agentReadiness !== undefined
        ? nextState.agentReadiness
        : current.agentReadiness,
    projects: nextState.projects !== undefined ? nextState.projects : current.projects,
    orchestrators:
      nextState.orchestrators !== undefined
        ? nextState.orchestrators
        : current.orchestrators,
    workspaces:
      nextState.workspaces !== undefined ? nextState.workspaces : current.workspaces,
  };
}

function createControlPanelSectionLauncherTab(
  sectionId: ControlPanelSectionId,
  options: {
    filesystemRoot: string | null;
    gitWorkdir: string | null;
    originProjectId: string | null;
    originSessionId: string | null;
  },
): WorkspaceTab | null {
  const { filesystemRoot, gitWorkdir, originProjectId, originSessionId } =
    options;
  switch (sectionId) {
    case "files":
      return (filesystemRoot?.trim() ?? "")
        ? createFilesystemTab(filesystemRoot, originSessionId, originProjectId)
        : null;
    case "git":
      return (gitWorkdir?.trim() ?? "")
        ? createGitStatusTab(gitWorkdir, originSessionId, originProjectId)
        : null;
    case "projects":
      return createProjectListTab(originSessionId, originProjectId);
    case "sessions":
      return createSessionListTab(originSessionId, originProjectId);
    case "orchestrators":
      return createOrchestratorListTab(originSessionId, originProjectId);
  }
}

function resolveWorkspaceScopedProjectId(
  originProjectId: string | null,
  originSessionId: string | null,
  sessionLookup: ReadonlyMap<string, Session>,
  projectLookup: ReadonlyMap<string, Project>,
) {
  const normalizedOriginProjectId = originProjectId?.trim() ?? "";
  if (
    normalizedOriginProjectId &&
    projectLookup.has(normalizedOriginProjectId)
  ) {
    return normalizedOriginProjectId;
  }

  const originSessionProjectId = originSessionId
    ? (sessionLookup.get(originSessionId)?.projectId?.trim() ?? "")
    : "";
  return originSessionProjectId && projectLookup.has(originSessionProjectId)
    ? originSessionProjectId
    : null;
}

function resolveWorkspaceScopedSessionId(
  projectId: string,
  preferredSessionId: string | null,
  activeSession: Session | null,
  sessions: readonly Session[],
  sessionLookup: ReadonlyMap<string, Session>,
) {
  const preferredSession = preferredSessionId
    ? (sessionLookup.get(preferredSessionId) ?? null)
    : null;
  if (preferredSession?.projectId === projectId) {
    return preferredSession.id;
  }

  if (activeSession?.projectId === projectId) {
    return activeSession.id;
  }

  return (
    sessions.find((session) => session.projectId === projectId)?.id ?? null
  );
}

function buildControlSurfaceSessionListState(
  sessions: readonly Session[],
  selectedProject: Project | null,
  sessionListFilter: SessionListFilter,
  sessionListSearchQuery: string,
) {
  const projectScopedSessions = selectedProject
    ? sessions.filter((session) => session.projectId === selectedProject.id)
    : sessions;
  const mutableProjectScopedSessions = [...projectScopedSessions];
  const sessionFilterCounts = countSessionsByFilter(
    mutableProjectScopedSessions,
  );
  const statusFilteredSessions = filterSessionsByListFilter(
    mutableProjectScopedSessions,
    sessionListFilter,
  );
  const trimmedSearchQuery = sessionListSearchQuery.trim();
  const hasSessionListSearch = trimmedSearchQuery.length > 0;

  if (!hasSessionListSearch) {
    return {
      projectScopedSessions,
      sessionFilterCounts,
      hasSessionListSearch,
      sessionListSearchResults: new Map<string, SessionListSearchResult>(),
      filteredSessions: statusFilteredSessions,
    };
  }

  const sessionListSearchResults = new Map(
    statusFilteredSessions.flatMap((session) => {
      const result = buildSessionListSearchResultFromIndex(
        buildSessionSearchIndex(session),
        trimmedSearchQuery,
      );
      return result ? ([[session.id, result]] as const) : [];
    }),
  );

  return {
    projectScopedSessions,
    sessionFilterCounts,
    hasSessionListSearch,
    sessionListSearchResults,
    filteredSessions: statusFilteredSessions.filter((session) =>
      sessionListSearchResults.has(session.id),
    ),
  };
}

type ControlSurfaceSessionListEntry =
  | { kind: "session"; session: Session }
  | {
      kind: "orchestratorGroup";
      orchestrator: OrchestratorInstance;
      sessions: Session[];
    };

export function formatSessionOrchestratorGroupName(
  orchestrator: OrchestratorInstance,
) {
  const trimmedName = orchestrator.templateSnapshot.name.trim();
  return trimmedName.length > 0 ? trimmedName : orchestrator.templateId;
}

export function buildControlSurfaceSessionListEntries(
  sessions: readonly Session[],
  orchestrators: readonly OrchestratorInstance[],
): ControlSurfaceSessionListEntry[] {
  if (!sessions.length) {
    return [];
  }

  if (!orchestrators.length) {
    return sessions.map((session) => ({ kind: "session", session }));
  }

  const sessionOrchestrators = new Map<string, OrchestratorInstance>();
  const orderedOrchestrators = [...orchestrators].sort((left, right) =>
    right.createdAt.localeCompare(left.createdAt),
  );

  for (const orchestrator of orderedOrchestrators) {
    for (const sessionInstance of orchestrator.sessionInstances) {
      if (!sessionOrchestrators.has(sessionInstance.sessionId)) {
        sessionOrchestrators.set(sessionInstance.sessionId, orchestrator);
      }
    }
  }

  const groupedSessionsByOrchestratorId = new Map<string, Session[]>();
  const entries: ControlSurfaceSessionListEntry[] = [];

  for (const session of sessions) {
    const orchestrator = sessionOrchestrators.get(session.id);

    if (!orchestrator) {
      entries.push({ kind: "session", session });
      continue;
    }

    const groupedSessions = groupedSessionsByOrchestratorId.get(orchestrator.id);
    if (groupedSessions) {
      groupedSessions.push(session);
      continue;
    }

    const nextGroupedSessions = [session];
    groupedSessionsByOrchestratorId.set(orchestrator.id, nextGroupedSessions);
    entries.push({
      kind: "orchestratorGroup",
      orchestrator,
      sessions: nextGroupedSessions,
    });
  }

  return entries;
}

function mergeOrchestratorDeltaSessions(
  previousSessions: Session[],
  deltaSessions: Session[] | undefined,
) {
  if (!deltaSessions?.length) {
    return previousSessions;
  }

  const deltaSessionsById = new Map(
    deltaSessions.map((session) => [session.id, session]),
  );
  const nextSessions = previousSessions.map(
    (session) => deltaSessionsById.get(session.id) ?? session,
  );
  const knownSessionIds = new Set(nextSessions.map((session) => session.id));
  for (const session of deltaSessions) {
    if (!knownSessionIds.has(session.id)) {
      nextSessions.push(session);
      knownSessionIds.add(session.id);
    }
  }

  return reconcileSessions(previousSessions, nextSessions);
}

export function syncMessageStackScrollPosition(
  node: Pick<HTMLElement, "scrollHeight" | "scrollTop" | "clientHeight">,
  scrollStateKey: string,
  paneScrollPositions: Record<string, { top: number; shouldStick: boolean }>,
) {
  const shouldStick = node.scrollHeight - node.scrollTop - node.clientHeight < 72;
  paneScrollPositions[scrollStateKey] = {
    top: node.scrollTop,
    shouldStick,
  };

  return {
    top: node.scrollTop,
    shouldStick,
  };
}

function getDockedControlPanelWidthRatioForWorkspace(
  workspace: WorkspaceState,
): number | null {
  const controlPanelPaneId =
    workspace.panes.find((pane) =>
      pane.tabs.some((tab) => tab.kind === "controlPanel"),
    )?.id ?? null;
  if (
    !controlPanelPaneId ||
    !workspace.root ||
    workspace.root.type !== "split" ||
    workspace.root.direction !== "row"
  ) {
    return null;
  }

  if (
    workspace.root.first.type === "pane" &&
    workspace.root.first.paneId === controlPanelPaneId
  ) {
    return workspace.root.ratio;
  }

  if (
    workspace.root.second.type === "pane" &&
    workspace.root.second.paneId === controlPanelPaneId
  ) {
    return 1 - workspace.root.ratio;
  }

  return null;
}

function resolvePreferredControlPanelWidthRatio(
  workspace: WorkspaceState,
): number {
  const minimumWidthRatio = resolveStandaloneControlPanelDockWidthRatio(
    DEFAULT_CONTROL_PANEL_DOCK_WIDTH_RATIO,
  );
  const currentWidthRatio = getDockedControlPanelWidthRatioForWorkspace(
    workspace,
  );

  return currentWidthRatio === null
    ? minimumWidthRatio
    : Math.max(currentWidthRatio, minimumWidthRatio);
}

function hydrateControlPanelLayout(
  workspace: WorkspaceState,
  side: ControlPanelSide,
): WorkspaceState {
  const workspaceWithControlPanel = ensureControlPanelInWorkspaceState(
    workspace,
  );

  return dockControlPanelAtWorkspaceEdge(
    workspaceWithControlPanel,
    side,
    resolvePreferredControlPanelWidthRatio(workspaceWithControlPanel),
  );
}

function createInitialWorkspaceBootstrap(workspaceViewId: string) {
  const storedLayout = getStoredWorkspaceLayout(workspaceViewId);
  const controlPanelSide: ControlPanelSide =
    storedLayout?.controlPanelSide ?? "left";
  const themeId: ThemeId = storedLayout?.themeId ?? getStoredThemePreference();
  const styleId: StyleId = storedLayout?.styleId ?? getStoredStylePreference();
  const fontSizePx = storedLayout?.fontSizePx ?? getStoredFontSizePreference();
  const editorFontSizePx =
    storedLayout?.editorFontSizePx ?? getStoredEditorFontSizePreference();
  const densityPercent =
    storedLayout?.densityPercent ?? getStoredDensityPreference();
  const workspace = hydrateControlPanelLayout(
    storedLayout?.workspace ?? {
      root: null,
      panes: [],
      activePaneId: null,
    },
    controlPanelSide,
  );

  return {
    controlPanelSide,
    themeId,
    styleId,
    fontSizePx,
    editorFontSizePx,
    densityPercent,
    workspace,
  };
}

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
  const [isWorkspaceLayoutReady, setIsWorkspaceLayoutReady] = useState(false);
  const [isWorkspaceSwitcherOpen, setIsWorkspaceSwitcherOpen] = useState(false);
  const [workspaceSummaries, setWorkspaceSummaries] = useState<
    WorkspaceLayoutSummary[]
  >([]);
  const [isWorkspaceSwitcherLoading, setIsWorkspaceSwitcherLoading] =
    useState(false);
  const [workspaceSwitcherError, setWorkspaceSwitcherError] = useState<
    string | null
  >(null);
  const [deletingWorkspaceIds, setDeletingWorkspaceIds] = useState<string[]>([]);
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
  const [
    pendingOrchestratorActionById,
    setPendingOrchestratorActionById,
  ] = useState<Record<string, OrchestratorRuntimeAction | undefined>>({});
  const [killingSessionIds, setKillingSessionIds] = useState<SessionFlagMap>(
    {},
  );
  const [killRevealSessionId, setKillRevealSessionId] = useState<string | null>(
    null,
  );
  const [pendingKillSessionId, setPendingKillSessionId] = useState<
    string | null
  >(null);
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
  const [backendInlineRequestErrorMessage, setBackendInlineRequestErrorMessage] =
    useState<string | null>(null);
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
  const [themeId, setThemeId] = useState<ThemeId>(
    initialWorkspaceBootstrap.themeId,
  );
  const [styleId, setStyleId] = useState<StyleId>(
    initialWorkspaceBootstrap.styleId,
  );
  const [fontSizePx, setFontSizePx] = useState<number>(
    initialWorkspaceBootstrap.fontSizePx,
  );
  const [editorFontSizePx, setEditorFontSizePx] = useState<number>(
    initialWorkspaceBootstrap.editorFontSizePx,
  );
  const [densityPercent, setDensityPercent] = useState<number>(
    initialWorkspaceBootstrap.densityPercent,
  );
  const [defaultCodexSandboxMode, setDefaultCodexSandboxMode] =
    useState<SandboxMode>("workspace-write");
  const [defaultCodexApprovalPolicy, setDefaultCodexApprovalPolicy] =
    useState<ApprovalPolicy>("never");
  const [defaultCodexReasoningEffort, setDefaultCodexReasoningEffort] =
    useState<CodexReasoningEffort>(DEFAULT_CODEX_REASONING_EFFORT);
  const [defaultClaudeApprovalMode, setDefaultClaudeApprovalMode] =
    useState<ClaudeApprovalMode>("ask");
  const [defaultClaudeEffort, setDefaultClaudeEffort] =
    useState<ClaudeEffortLevel>(DEFAULT_CLAUDE_EFFORT);
  const [remoteConfigs, setRemoteConfigs] = useState<RemoteConfig[]>(
    () => resolveAppPreferences(null).remotes,
  );
  const [defaultCursorMode, setDefaultCursorMode] =
    useState<CursorMode>("agent");
  const [defaultGeminiApprovalMode, setDefaultGeminiApprovalMode] =
    useState<GeminiApprovalMode>("default");
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
  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState<PreferencesTabId>("themes");
  const [pendingSessionRename, setPendingSessionRename] =
    useState<PendingSessionRename | null>(null);
  const [pendingSessionRenameDraft, setPendingSessionRenameDraft] =
    useState("");
  const [pendingSessionRenameStyle, setPendingSessionRenameStyle] =
    useState<CSSProperties | null>(null);
  const [pendingKillPopoverStyle, setPendingKillPopoverStyle] =
    useState<CSSProperties | null>(null);
  const [pendingScrollToBottomRequest, setPendingScrollToBottomRequest] =
    useState<{
      sessionId: string;
      token: number;
    } | null>(null);
  const [windowId] = useState(() => crypto.randomUUID());
  const [draggedTab, setDraggedTab] = useState<WorkspaceTabDrag | null>(null);
  const [workspaceFilesChangedEvent, setWorkspaceFilesChangedEvent] =
    useState<WorkspaceFilesChangedEvent | null>(null);
  const workspaceFilesChangedEventBufferRef =
    useRef<WorkspaceFilesChangedEvent | null>(null);
  const workspaceFilesChangedEventFlushTimeoutRef = useRef<number | null>(null);
  const lastWorkspaceFilesChangedRevisionRef = useRef<number | null>(null);
  const gitDiffPreviewRefreshVersionsRef = useRef<Map<string, number>>(new Map());
  const [launcherDraggedTab, setLauncherDraggedTab] =
    useState<WorkspaceTabDrag | null>(null);
  const [externalDraggedTab, setExternalDraggedTab] =
    useState<WorkspaceTabDrag | null>(null);
  const resizeStateRef = useRef<{
    splitId: string;
    direction: "row" | "column";
    startRatio: number;
    minRatio: number;
    maxRatio: number;
    startX: number;
    startY: number;
    size: number;
  } | null>(null);
  const ignoreFetchedWorkspaceLayoutRef = useRef(false);
  const backendInlineRequestErrorMessageRef = useRef<string | null>(null);
  const workspaceLayoutRestartErrorMessageRef = useRef<string | null>(null);
  const workspaceLayoutLoadPendingRef = useRef(false);
  const draftAttachmentsRef = useRef<Record<string, DraftImageAttachment[]>>(
    {},
  );
  const dragChannelRef = useRef<BroadcastChannel | null>(null);
  const draggedTabRef = useRef<WorkspaceTabDrag | null>(null);
  const launcherDraggedTabRef = useRef<WorkspaceTabDrag | null>(null);
  const isMountedRef = useRef(true);
  const activePromptPollIntervalRef = useRef<ReturnType<
    typeof setInterval
  > | null>(null);
  const activePromptPollTimeoutRef = useRef<ReturnType<
    typeof setTimeout
  > | null>(null);
  const sessionListSearchInputRef = useRef<HTMLInputElement>(null);
  const pendingSessionRenameTriggerRef = useRef<HTMLElement | null>(null);
  const pendingSessionRenamePopoverRef = useRef<HTMLFormElement | null>(null);
  const pendingSessionRenameInputRef = useRef<HTMLInputElement | null>(null);
  const pendingSessionRenameCloseTimeoutRef = useRef<number | null>(null);
  const pendingKillTriggerRef = useRef<HTMLButtonElement | null>(null);
  const pendingKillPopoverRef = useRef<HTMLDivElement | null>(null);
  const pendingKillConfirmButtonRef = useRef<HTMLButtonElement | null>(null);
  const pendingKillCloseTimeoutRef = useRef<number | null>(null);
  const confirmedUnknownModelSendsRef = useRef<Set<string>>(new Set());
  const refreshingSessionModelOptionIdsRef = useRef<SessionFlagMap>({});
  const refreshingAgentCommandSessionIdsRef = useRef<SessionFlagMap>({});
  const controlPanelSurfaceRef = useRef<ControlPanelSurfaceHandle | null>(null);
  const lastDerivedControlPanelFilesystemRootRef = useRef<string | null>(null);
  const lastDerivedControlPanelGitWorkdirRef = useRef<string | null>(null);
  const workspaceSwitcherRef = useRef<HTMLDivElement | null>(null);
  const workspaceSummariesRequestTokenRef = useRef(0);
  const deletingWorkspaceIdsRef = useRef<Set<string>>(new Set());
  const pendingWorkspaceLayoutSaveRef =
    useRef<PendingWorkspaceLayoutSave | null>(null);
  const pendingWorkspaceLayoutSaveTimeoutRef = useRef<number | null>(null);
  const flushWorkspaceLayoutSaveRef = useRef<
    (options?: { keepalive?: boolean }) => void
  >(() => {});
  const sessionsRef = useRef<Session[]>([]);
  const workspaceRef = useRef(workspace);
  const codexStateRef = useRef(codexState);
  const agentReadinessRef = useRef(agentReadiness);
  const projectsRef = useRef(projects);
  const orchestratorsRef = useRef(orchestrators);
  const workspaceSummariesRef = useRef(workspaceSummaries);
  const latestStateRevisionRef = useRef<number | null>(null);
  const forceAdoptNextStateEventRef = useRef(false);
  const stateResyncInFlightRef = useRef(false);
  const stateResyncPendingRef = useRef(false);
  const stateResyncAllowAuthoritativeRollbackRef = useRef(false);
  const stateResyncPreserveReconnectFallbackRef = useRef(false);
  const stateResyncPreserveWatchdogCooldownRef = useRef(false);
  const stateResyncRearmOnSuccessRef = useRef(false);
  const stateResyncRearmOnFailureRef = useRef(false);
  const requestBackendReconnectRef = useRef<() => void>(() => {});
  const requestActionRecoveryResyncRef = useRef<() => void>(() => {});
  const syncAdoptedLiveSessionResumeWatchdogBaselinesRef = useRef<
    (sessions: Session[], now?: number) => void
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
      options?.message ?? (typeof error === "string" ? error : getErrorMessage(error));
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

    // Incompatible backend serving HTML — show the restart instruction in the
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
    // badge because the action-recovery resync only clears error text — only
    // EventSource.onopen / confirmReconnectRecoveryFromLiveEvent restore
    // "connected", and those won't fire when the stream never went down.
    //
    // The inline request error + issue detail are enough to surface the error.
    // The action-recovery probe will clear both on success.
    requestActionRecoveryResyncRef.current();
  }

  const clearRecoveredBackendRequestError = useCallback(() => {
    const inlineRequestErrorMessage = backendInlineRequestErrorMessageRef.current;
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
  const selectedProject =
    selectedProjectId === ALL_PROJECTS_FILTER_ID
      ? null
      : (projectLookup.get(selectedProjectId) ?? null);
  const remoteLookup = useMemo(
    () => new Map(remoteConfigs.map((remote) => [remote.id, remote])),
    [remoteConfigs],
  );
  const localRemoteConfig =
    remoteLookup.get(LOCAL_REMOTE_ID) ?? createBuiltinLocalRemote();
  const enabledProjectRemotes = useMemo(
    () =>
      remoteConfigs.filter(
        (remote) => remote.enabled || isLocalRemoteId(remote.id),
      ),
    [remoteConfigs],
  );
  const newProjectSelectedRemote = resolveRemoteConfig(
    remoteLookup,
    newProjectRemoteId,
  );
  const newProjectUsesLocalRemote = isLocalRemoteId(newProjectRemoteId);
  const createProjectRemoteOptions = useMemo<readonly ComboboxOption[]>(() => {
    return enabledProjectRemotes.map((remote) => ({
      label: remoteDisplayName(remote, remote.id),
      value: remote.id,
      description: remoteConnectionLabel(remote),
      badges: [remoteBadgeLabel(remote)],
    }));
  }, [enabledProjectRemotes]);
  const newSessionModelOptions = NEW_SESSION_MODEL_OPTIONS[newSessionAgent];
  const newSessionModel =
    newSessionModelByAgent[newSessionAgent] ??
    defaultNewSessionModel(newSessionAgent);
  const createSessionSelectedProject =
    createSessionProjectId === CREATE_SESSION_WORKSPACE_ID
      ? null
      : (projectLookup.get(createSessionProjectId) ?? null);
  const createSessionWorkspaceProject =
    createSessionProjectId === CREATE_SESSION_WORKSPACE_ID &&
    activeSession?.projectId &&
    projectLookup.has(activeSession.projectId)
      ? (projectLookup.get(activeSession.projectId) ?? null)
      : null;
  const createSessionEffectiveProject =
    createSessionSelectedProject ??
    (createSessionWorkspaceProject &&
    !isLocalRemoteId(resolveProjectRemoteId(createSessionWorkspaceProject))
      ? createSessionWorkspaceProject
      : null);
  const createSessionSelectedRemote = createSessionEffectiveProject
    ? resolveRemoteConfig(
        remoteLookup,
        resolveProjectRemoteId(createSessionEffectiveProject),
      )
    : localRemoteConfig;
  const createSessionProjectOptions = useMemo<readonly ComboboxOption[]>(() => {
    const workspaceLabel = activeSession?.workdir
      ? "Current workspace"
      : "Default workspace";

    return [
      { label: workspaceLabel, value: CREATE_SESSION_WORKSPACE_ID },
      ...projects.map((project) => {
        const remote = resolveRemoteConfig(
          remoteLookup,
          resolveProjectRemoteId(project),
        );
        return {
          label: project.name,
          value: project.id,
          description: describeProjectScope(project, remoteLookup),
          badges: [remoteBadgeLabel(remote)],
        };
      }),
    ];
  }, [activeSession?.workdir, projects, remoteLookup]);
  const controlPanelProjectOptions = useMemo<readonly ComboboxOption[]>(() => {
    return [
      {
        label: "All projects",
        value: ALL_PROJECTS_FILTER_ID,
        description: "Show every session in this window.",
      },
      ...projects.map((project) => {
        const remote = resolveRemoteConfig(
          remoteLookup,
          resolveProjectRemoteId(project),
        );
        return {
          label: project.name,
          value: project.id,
          description: describeProjectScope(project, remoteLookup),
          badges: [remoteBadgeLabel(remote)],
        };
      }),
    ];
  }, [projects, remoteLookup]);
  const createSessionProjectHint = createSessionSelectedProject
    ? describeProjectScope(createSessionSelectedProject, remoteLookup)
    : createSessionEffectiveProject
      ? describeProjectScope(createSessionEffectiveProject, remoteLookup)
      : activeSession?.workdir
        ? `Uses ${activeSession.workdir}`
        : "Uses the app default workspace.";
  const createSessionUsesRemoteProject =
    !!createSessionEffectiveProject &&
    !isLocalRemoteId(resolveProjectRemoteId(createSessionEffectiveProject));
  const createSessionProjectSelectionError =
    createSessionProjectId === CREATE_SESSION_WORKSPACE_ID &&
    !!activeSession?.projectId &&
    !projectLookup.has(activeSession.projectId)
      ? "The current workspace is tied to a project that is no longer available. Choose a project before creating a session."
      : null;
  const createSessionUsesSessionModelPicker =
    usesSessionModelPicker(newSessionAgent);
  const createSessionAgentReadiness = createSessionUsesRemoteProject
    ? null
    : (agentReadinessByAgent.get(newSessionAgent) ?? null);
  const createSessionBlocked = createSessionAgentReadiness?.blocking ?? false;
  const projectScopedSessions = useMemo(() => {
    if (!selectedProject) {
      return sessions;
    }

    return sessions.filter(
      (session) => session.projectId === selectedProject.id,
    );
  }, [selectedProject, sessions]);
  const dockedControlPanelPane =
    workspace.panes.find((pane) =>
      pane.tabs.some((tab) => tab.kind === "controlPanel"),
    ) ?? null;
  const dockedControlPanelActiveTab = dockedControlPanelPane
    ? (dockedControlPanelPane.tabs.find(
        (tab) => tab.id === dockedControlPanelPane.activeTabId,
      ) ??
      dockedControlPanelPane.tabs[0] ??
      null)
    : null;
  const dockedControlPanelOriginSession =
    dockedControlPanelActiveTab &&
    "originSessionId" in dockedControlPanelActiveTab &&
    dockedControlPanelActiveTab.originSessionId
      ? (sessionLookup.get(dockedControlPanelActiveTab.originSessionId) ?? null)
      : null;
  const dockedControlPanelPaneSession = dockedControlPanelPane?.activeSessionId
    ? (sessionLookup.get(dockedControlPanelPane.activeSessionId) ?? null)
    : null;
  const dockedControlPanelNearestSessionPaneId = dockedControlPanelPane
    ? findNearestSessionPaneId(workspace, dockedControlPanelPane.id)
    : null;
  const dockedControlPanelNearestSessionPane =
    dockedControlPanelNearestSessionPaneId
      ? (paneLookup.get(dockedControlPanelNearestSessionPaneId) ?? null)
      : null;
  const dockedControlPanelNearestSessionTab =
    dockedControlPanelNearestSessionPane
      ? (dockedControlPanelNearestSessionPane.tabs.find(
          (tab) => tab.id === dockedControlPanelNearestSessionPane.activeTabId,
        ) ??
        dockedControlPanelNearestSessionPane.tabs[0] ??
        null)
      : null;
  const dockedControlPanelNearestSession =
    dockedControlPanelNearestSessionTab?.kind === "session"
      ? (sessionLookup.get(dockedControlPanelNearestSessionTab.sessionId) ??
        null)
      : null;
  const dockedControlPanelSessionCandidates = [
    dockedControlPanelOriginSession,
    dockedControlPanelPaneSession,
    dockedControlPanelNearestSession,
    activeSession,
  ].filter((session): session is Session => Boolean(session));
  const controlPanelContextSession = selectedProject
    ? (dockedControlPanelSessionCandidates.find(
        (session) => session.projectId === selectedProject.id,
      ) ??
      projectScopedSessions[0] ??
      null)
    : (dockedControlPanelSessionCandidates[0] ?? sessions[0] ?? null);
  const derivedControlPanelWorkspaceRoot = resolveControlPanelWorkspaceRoot(
    selectedProject,
    controlPanelContextSession?.workdir ?? null,
  );
  const derivedControlPanelFilesystemRoot = derivedControlPanelWorkspaceRoot;
  const derivedControlPanelGitWorkdir = derivedControlPanelWorkspaceRoot;
  const controlPanelSessionId = controlPanelContextSession?.id ?? null;
  const sessionFilterCounts = useMemo(
    () => countSessionsByFilter(projectScopedSessions),
    [projectScopedSessions],
  );
  const statusFilteredSessions = useMemo(() => {
    return filterSessionsByListFilter(projectScopedSessions, sessionListFilter);
  }, [projectScopedSessions, sessionListFilter]);
  const trimmedSessionListSearchQuery = sessionListSearchQuery.trim();
  const deferredSessionListSearchQuery = useDeferredValue(
    trimmedSessionListSearchQuery,
  );
  const effectiveSessionListSearchQuery =
    trimmedSessionListSearchQuery.length === 0
      ? ""
      : deferredSessionListSearchQuery;
  const hasSessionListSearch = effectiveSessionListSearchQuery.length > 0;
  const sessionListSearchIndex = useMemo(() => {
    if (!hasSessionListSearch) {
      return null;
    }

    return new Map(
      statusFilteredSessions.map(
        (session) => [session.id, buildSessionSearchIndex(session)] as const,
      ),
    );
  }, [hasSessionListSearch, statusFilteredSessions]);

  useEffect(() => {
    if (
      createSessionProjectId !== CREATE_SESSION_WORKSPACE_ID &&
      !projectLookup.has(createSessionProjectId)
    ) {
      setCreateSessionProjectId(CREATE_SESSION_WORKSPACE_ID);
    }
  }, [createSessionProjectId, projectLookup]);

  useEffect(() => {
    if (
      !enabledProjectRemotes.some((remote) => remote.id === newProjectRemoteId)
    ) {
      setNewProjectRemoteId(enabledProjectRemotes[0]?.id ?? LOCAL_REMOTE_ID);
    }
  }, [enabledProjectRemotes, newProjectRemoteId]);

  useEffect(() => {
    const previousDerived =
      lastDerivedControlPanelFilesystemRootRef.current?.trim() ?? "";
    lastDerivedControlPanelFilesystemRootRef.current =
      derivedControlPanelFilesystemRoot;

    setControlPanelFilesystemRoot((current) => {
      const trimmedCurrent = current?.trim() ?? "";
      if (!trimmedCurrent || trimmedCurrent === previousDerived) {
        return derivedControlPanelFilesystemRoot;
      }
      return current;
    });
  }, [derivedControlPanelFilesystemRoot]);

  useEffect(() => {
    const previousDerived =
      lastDerivedControlPanelGitWorkdirRef.current?.trim() ?? "";
    lastDerivedControlPanelGitWorkdirRef.current =
      derivedControlPanelGitWorkdir;

    setControlPanelGitWorkdir((current) => {
      const trimmedCurrent = current?.trim() ?? "";
      if (!trimmedCurrent || trimmedCurrent === previousDerived) {
        return derivedControlPanelGitWorkdir;
      }
      return current;
    });
  }, [derivedControlPanelGitWorkdir]);

  useEffect(() => {
    const normalizedGitWorkdir = controlPanelGitWorkdir?.trim() ?? "";
    let cancelled = false;

    if (!normalizedGitWorkdir) {
      setControlPanelGitStatusCount(0);
      return;
    }

    void fetchGitStatus(normalizedGitWorkdir, controlPanelSessionId, {
      projectId: selectedProject?.id ?? null,
    })
      .then((status) => {
        if (cancelled) {
          return;
        }
        setControlPanelGitStatusCount(status.files.length);
      })
      .catch(() => {
        if (cancelled) {
          return;
        }
        setControlPanelGitStatusCount(0);
      });

    return () => {
      cancelled = true;
    };
  }, [controlPanelGitWorkdir, controlPanelSessionId, selectedProject?.id]);

  const sessionListSearchResults = useMemo(() => {
    if (!hasSessionListSearch || !sessionListSearchIndex) {
      return new Map<string, SessionListSearchResult>();
    }

    return new Map(
      statusFilteredSessions.flatMap((session) => {
        const searchIndex = sessionListSearchIndex.get(session.id);
        if (!searchIndex) {
          return [];
        }

        const result = buildSessionListSearchResultFromIndex(
          searchIndex,
          effectiveSessionListSearchQuery,
        );
        return result ? ([[session.id, result]] as const) : [];
      }),
    );
  }, [
    effectiveSessionListSearchQuery,
    hasSessionListSearch,
    sessionListSearchIndex,
    statusFilteredSessions,
  ]);
  const filteredSessions = useMemo(() => {
    if (!hasSessionListSearch) {
      return statusFilteredSessions;
    }

    return statusFilteredSessions.filter((session) =>
      sessionListSearchResults.has(session.id),
    );
  }, [hasSessionListSearch, sessionListSearchResults, statusFilteredSessions]);
  const projectSessionCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const session of sessions) {
      if (!session.projectId) {
        continue;
      }
      counts.set(session.projectId, (counts.get(session.projectId) ?? 0) + 1);
    }
    return counts;
  }, [sessions]);
  const activeTheme = THEMES.find((theme) => theme.id === themeId) ?? THEMES[0];
  const activeStyle = STYLES.find((style) => style.id === styleId) ?? STYLES[0];
  const editorAppearance: MonacoAppearance = isHexColorDark(
    activeTheme.swatches[0],
  )
    ? "dark"
    : "light";
  const activeDraggedTab =
    draggedTab ?? launcherDraggedTab ?? externalDraggedTab;

  function getKnownWorkspaceTabDrag() {
    return (
      draggedTabRef.current ??
      draggedTab ??
      launcherDraggedTabRef.current ??
      launcherDraggedTab ??
      externalDraggedTab
    );
  }
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

  function broadcastTabDragMessage(message: WorkspaceTabDragChannelMessage) {
    dragChannelRef.current?.postMessage(message);
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

  function adoptSessions(
    nextSessions: Session[],
    options?: { openSessionId?: string; paneId?: string | null },
  ) {
    const previousSessions = sessionsRef.current;
    const previousSessionsById = new Map(
      previousSessions.map((session) => [session.id, session]),
    );
    const mergedSessions = reconcileSessions(previousSessions, nextSessions);
    const availableSessionIds = new Set(
      mergedSessions.map((session) => session.id),
    );
    const sessionsWithChangedWorkdir = new Set(
      mergedSessions.flatMap((session) => {
        const previousSession = previousSessionsById.get(session.id);
        return previousSession && previousSession.workdir !== session.workdir
          ? [session.id]
          : [];
      }),
    );
    // Avoid rewriting workspace state when an adopted snapshot preserves the
    // same reconciled sessions. Workspace autosave is keyed off `workspace`
    // identity, so an identity-only rewrite here can create a loop:
    // workspace PUT -> SSE state snapshot -> adoptSessions -> workspace save.
    const shouldReconcileWorkspace =
      mergedSessions !== previousSessions || Boolean(options?.openSessionId);

    sessionsRef.current = mergedSessions;
    setSessions(mergedSessions);
    if (shouldReconcileWorkspace) {
      setWorkspace((current) => {
        const reconciled =
          mergedSessions !== previousSessions
            ? applyControlPanelLayout(
                reconcileWorkspaceState(current, mergedSessions),
              )
            : current;
        if (!options?.openSessionId) {
          return reconciled;
        }

        return applyControlPanelLayout(
          openSessionInWorkspaceState(
            reconciled,
            options.openSessionId,
            options.paneId ?? null,
          ),
        );
      });
    }
    setDraftsBySessionId((current) =>
      pruneSessionValues(current, availableSessionIds),
    );
    setDraftAttachmentsBySessionId((current) =>
      pruneSessionAttachmentValues(current, availableSessionIds),
    );
    setSendingSessionIds((current) =>
      pruneSessionFlags(current, availableSessionIds),
    );
    setStoppingSessionIds((current) =>
      pruneSessionFlags(current, availableSessionIds),
    );
    setKillingSessionIds((current) =>
      pruneSessionFlags(current, availableSessionIds),
    );
    setKillRevealSessionId((current) =>
      current && availableSessionIds.has(current) ? current : null,
    );
    setPendingKillSessionId((current) =>
      current && availableSessionIds.has(current) ? current : null,
    );
    setPendingSessionRename((current) =>
      current && availableSessionIds.has(current.sessionId) ? current : null,
    );
    setUpdatingSessionIds((current) =>
      pruneSessionFlags(current, availableSessionIds),
    );
    setAgentCommandsBySessionId((current) =>
      pruneSessionCommandValues(
        current,
        availableSessionIds,
        sessionsWithChangedWorkdir,
      ),
    );
    setRefreshingAgentCommandSessionIds((current) =>
      pruneSessionFlagsWithInvalidation(
        current,
        availableSessionIds,
        sessionsWithChangedWorkdir,
      ),
    );
    refreshingAgentCommandSessionIdsRef.current =
      pruneSessionFlagsWithInvalidation(
        refreshingAgentCommandSessionIdsRef.current,
        availableSessionIds,
        sessionsWithChangedWorkdir,
      );
    setAgentCommandErrors((current) =>
      pruneSessionValues(
        current,
        availableSessionIds,
        sessionsWithChangedWorkdir,
      ),
    );
    setSessionSettingNotices((current) =>
      pruneSessionValues(current, availableSessionIds),
    );
    const availableUnknownModelKeys = new Set(
      mergedSessions
        .filter((session) => describeUnknownSessionModelWarning(session))
        .map((session) =>
          unknownSessionModelConfirmationKey(session.id, session.model),
        ),
    );
    confirmedUnknownModelSendsRef.current = new Set(
      [...confirmedUnknownModelSendsRef.current].filter((key) =>
        availableUnknownModelKeys.has(key),
      ),
    );
  }

  function updateSessionLocally(
    sessionId: string,
    update: (session: Session) => Session,
  ) {
    const nextSessions = sessionsRef.current.map((entry) => {
      if (entry.id !== sessionId) {
        return entry;
      }

      return update(entry);
    });
    const hasChanged = nextSessions.some(
      (entry, index) => entry !== sessionsRef.current[index],
    );
    if (!hasChanged) {
      return;
    }

    sessionsRef.current = nextSessions;
    setSessions(nextSessions);
    setWorkspace((current) =>
      applyControlPanelLayout(reconcileWorkspaceState(current, nextSessions)),
    );
  }

  function beginWorkspaceSummariesRequest() {
    workspaceSummariesRequestTokenRef.current += 1;
    return workspaceSummariesRequestTokenRef.current;
  }

  function isLatestWorkspaceSummariesRequest(requestToken: number) {
    return workspaceSummariesRequestTokenRef.current === requestToken;
  }

  function finishDeletingWorkspace(workspaceId: string) {
    const nextDeletingWorkspaceIds = new Set(deletingWorkspaceIdsRef.current);
    nextDeletingWorkspaceIds.delete(workspaceId);
    deletingWorkspaceIdsRef.current = nextDeletingWorkspaceIds;
    if (isMountedRef.current) {
      setDeletingWorkspaceIds([...nextDeletingWorkspaceIds]);
    }
  }

  const refreshWorkspaceSummaries = useCallback(async () => {
    const requestToken = beginWorkspaceSummariesRequest();
    const workspacesAtRequest = workspaceSummariesRef.current;
    setIsWorkspaceSwitcherLoading(true);
    setWorkspaceSwitcherError(null);
    try {
      const response = await fetchWorkspaceLayouts();
      if (
        !isMountedRef.current ||
        !isLatestWorkspaceSummariesRequest(requestToken)
      ) {
        return;
      }
      // Only apply the refresh result when the workspace list has not been
      // updated by another source (SSE-delivered workspace data, a delete
      // handler, etc.) during the fetch. This avoids overwriting a more
      // authoritative SSE-delivered list with a stale /api/workspaces
      // snapshot, while still applying the result when only unrelated
      // session/orchestrator events arrived.
      if (workspaceSummariesRef.current === workspacesAtRequest) {
        workspaceSummariesRef.current = response.workspaces;
        setWorkspaceSummaries(response.workspaces);
      }
    } catch (error) {
      if (
        !isMountedRef.current ||
        !isLatestWorkspaceSummariesRequest(requestToken)
      ) {
        return;
      }
      setWorkspaceSwitcherError(getErrorMessage(error));
    } finally {
      if (
        isMountedRef.current &&
        isLatestWorkspaceSummariesRequest(requestToken)
      ) {
        setIsWorkspaceSwitcherLoading(false);
      }
    }
    // All dependencies are stable callbacks or refs, so re-subscribing only
    // happens if the browser-recovery handler itself changes.
  }, [clearRecoveredBackendRequestError, setBackendConnectionState]);

  function clearPendingWorkspaceLayoutSaveTimeout() {
    if (
      pendingWorkspaceLayoutSaveTimeoutRef.current === null ||
      typeof window === "undefined"
    ) {
      return;
    }

    window.clearTimeout(pendingWorkspaceLayoutSaveTimeoutRef.current);
    pendingWorkspaceLayoutSaveTimeoutRef.current = null;
  }

  function persistPendingWorkspaceLayoutSave(
    pendingSave: PendingWorkspaceLayoutSave,
    options?: { keepalive?: boolean },
  ) {
    void saveWorkspaceLayout(
      pendingSave.workspaceId,
      pendingSave.layout,
      options?.keepalive ? { keepalive: true } : undefined,
    ).catch((error) => {
      console.warn(
        "workspace layout warning> failed to save server workspace layout:",
        error,
      );
    });
  }

  function flushPendingWorkspaceLayoutSave(options?: { keepalive?: boolean }) {
    clearPendingWorkspaceLayoutSaveTimeout();
    const pendingSave = pendingWorkspaceLayoutSaveRef.current;
    if (!pendingSave) {
      return;
    }

    pendingWorkspaceLayoutSaveRef.current = null;
    persistPendingWorkspaceLayoutSave(pendingSave, options);
  }

  flushWorkspaceLayoutSaveRef.current = flushPendingWorkspaceLayoutSave;

  function navigateToWorkspace(nextWorkspaceViewId: string) {
    if (typeof window === "undefined") {
      return;
    }

    flushPendingWorkspaceLayoutSave({ keepalive: true });
    const url = new URL(window.location.href);
    url.searchParams.set(WORKSPACE_VIEW_QUERY_PARAM, nextWorkspaceViewId);
    window.location.assign(url.toString());
  }

  function handleWorkspaceSwitcherToggle() {
    setIsWorkspaceSwitcherOpen((current) => !current);
  }

  function handleOpenWorkspaceHere(nextWorkspaceViewId: string) {
    setIsWorkspaceSwitcherOpen(false);
    if (nextWorkspaceViewId === workspaceViewId) {
      return;
    }
    navigateToWorkspace(nextWorkspaceViewId);
  }

  function handleOpenNewWorkspaceHere() {
    handleOpenWorkspaceHere(createWorkspaceViewId());
  }

  function handleOpenNewWorkspaceWindow() {
    if (typeof window === "undefined") {
      return;
    }

    const nextWorkspaceViewId = createWorkspaceViewId();
    flushPendingWorkspaceLayoutSave({ keepalive: true });
    const url = new URL(window.location.href);
    url.searchParams.set(WORKSPACE_VIEW_QUERY_PARAM, nextWorkspaceViewId);
    window.open(url.toString(), "_blank", "noopener");
    setIsWorkspaceSwitcherOpen(false);
  }

  async function handleDeleteWorkspace(workspaceId: string) {
    if (
      workspaceId === workspaceViewId ||
      deletingWorkspaceIdsRef.current.has(workspaceId)
    ) {
      return;
    }

    const nextDeletingWorkspaceIds = new Set(deletingWorkspaceIdsRef.current);
    nextDeletingWorkspaceIds.add(workspaceId);
    deletingWorkspaceIdsRef.current = nextDeletingWorkspaceIds;
    setDeletingWorkspaceIds([...nextDeletingWorkspaceIds]);
    setWorkspaceSwitcherError(null);

    const requestToken = beginWorkspaceSummariesRequest();
    const workspacesAtRequest = workspaceSummariesRef.current;
    setIsWorkspaceSwitcherLoading(true);
    try {
      const deleteResponse = await deleteWorkspaceLayout(workspaceId);
      deleteStoredWorkspaceLayout(workspaceId);
      if (isMountedRef.current) {
        if (
          isLatestWorkspaceSummariesRequest(requestToken) &&
          workspaceSummariesRef.current === workspacesAtRequest
        ) {
          // This is the latest workspace request and the workspace list
          // has not been updated by another source (SSE, another delete,
          // a refresh) during the flight: the server's post-delete list is
          // the most up-to-date view and safely reflects concurrent
          // cross-tab operations.
          workspaceSummariesRef.current = deleteResponse.workspaces;
          setWorkspaceSummaries(deleteResponse.workspaces);
        } else {
          // Either a newer workspace request was initiated (e.g. a refresh)
          // or the workspace list was updated by SSE / another handler
          // during the delete. Don't replace the entire list (the newer
          // source is more authoritative), but ensure the confirmed-deleted
          // workspace is removed locally.
          setWorkspaceSummaries((current) => {
            const next = current.filter((w) => w.id !== workspaceId);
            workspaceSummariesRef.current = next;
            return next;
          });
        }
      }
    } catch (error) {
      if (
        isMountedRef.current &&
        isLatestWorkspaceSummariesRequest(requestToken)
      ) {
        setWorkspaceSwitcherError(getErrorMessage(error));
      }
    } finally {
      finishDeletingWorkspace(workspaceId);
      if (
        isMountedRef.current &&
        isLatestWorkspaceSummariesRequest(requestToken)
      ) {
        setIsWorkspaceSwitcherLoading(false);
      }
    }
  }

  function buildOptimisticSessionSettingsUpdate(
    session: Session,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) {
    const normalizedModelValue =
      field === "model"
        ? normalizedRequestedSessionModel(session, value as string)
        : null;

    switch (session.agent) {
      case "Codex": {
        const nextModel = normalizedModelValue ?? session.model;
        const nextReasoningEffort =
          field === "reasoningEffort"
            ? (value as CodexReasoningEffort)
            : normalizedCodexReasoningEffort(session, nextModel);
        const nextSandboxMode =
          field === "sandboxMode"
            ? (value as SandboxMode)
            : session.sandboxMode;
        const nextApprovalPolicy =
          field === "approvalPolicy"
            ? (value as ApprovalPolicy)
            : session.approvalPolicy;

        if (
          nextModel === session.model &&
          nextReasoningEffort === session.reasoningEffort &&
          nextSandboxMode === session.sandboxMode &&
          nextApprovalPolicy === session.approvalPolicy
        ) {
          return session;
        }

        return {
          ...session,
          model: nextModel,
          reasoningEffort: nextReasoningEffort,
          sandboxMode: nextSandboxMode,
          approvalPolicy: nextApprovalPolicy,
        };
      }
      case "Cursor": {
        const nextModel = normalizedModelValue ?? session.model;
        const nextCursorMode =
          field === "cursorMode" ? (value as CursorMode) : session.cursorMode;

        if (
          nextModel === session.model &&
          nextCursorMode === session.cursorMode
        ) {
          return session;
        }

        return {
          ...session,
          model: nextModel,
          cursorMode: nextCursorMode,
        };
      }
      case "Claude": {
        const nextModel = normalizedModelValue ?? session.model;
        const nextClaudeApprovalMode =
          field === "claudeApprovalMode"
            ? (value as ClaudeApprovalMode)
            : session.claudeApprovalMode;
        const nextClaudeEffort =
          field === "claudeEffort"
            ? (value as ClaudeEffortLevel)
            : session.claudeEffort;

        if (
          nextModel === session.model &&
          nextClaudeApprovalMode === session.claudeApprovalMode &&
          nextClaudeEffort === session.claudeEffort
        ) {
          return session;
        }

        return {
          ...session,
          model: nextModel,
          claudeApprovalMode: nextClaudeApprovalMode,
          claudeEffort: nextClaudeEffort,
        };
      }
      case "Gemini": {
        const nextModel = normalizedModelValue ?? session.model;
        const nextGeminiApprovalMode =
          field === "geminiApprovalMode"
            ? (value as GeminiApprovalMode)
            : session.geminiApprovalMode;

        if (
          nextModel === session.model &&
          nextGeminiApprovalMode === session.geminiApprovalMode
        ) {
          return session;
        }

        return {
          ...session,
          model: nextModel,
          geminiApprovalMode: nextGeminiApprovalMode,
        };
      }
    }
  }

  function rollbackOptimisticSessionSettingsUpdate(
    currentSession: Session,
    previousSession: Session,
    optimisticSession: Session,
  ) {
    let changed = false;
    const nextSession = { ...currentSession };

    if (
      currentSession.model === optimisticSession.model &&
      currentSession.model !== previousSession.model
    ) {
      nextSession.model = previousSession.model;
      changed = true;
    }
    if (
      currentSession.approvalPolicy === optimisticSession.approvalPolicy &&
      currentSession.approvalPolicy !== previousSession.approvalPolicy
    ) {
      nextSession.approvalPolicy = previousSession.approvalPolicy;
      changed = true;
    }
    if (
      currentSession.reasoningEffort === optimisticSession.reasoningEffort &&
      currentSession.reasoningEffort !== previousSession.reasoningEffort
    ) {
      nextSession.reasoningEffort = previousSession.reasoningEffort;
      changed = true;
    }
    if (
      currentSession.sandboxMode === optimisticSession.sandboxMode &&
      currentSession.sandboxMode !== previousSession.sandboxMode
    ) {
      nextSession.sandboxMode = previousSession.sandboxMode;
      changed = true;
    }
    if (
      currentSession.cursorMode === optimisticSession.cursorMode &&
      currentSession.cursorMode !== previousSession.cursorMode
    ) {
      nextSession.cursorMode = previousSession.cursorMode;
      changed = true;
    }
    if (
      currentSession.claudeApprovalMode ===
        optimisticSession.claudeApprovalMode &&
      currentSession.claudeApprovalMode !== previousSession.claudeApprovalMode
    ) {
      nextSession.claudeApprovalMode = previousSession.claudeApprovalMode;
      changed = true;
    }
    if (
      currentSession.claudeEffort === optimisticSession.claudeEffort &&
      currentSession.claudeEffort !== previousSession.claudeEffort
    ) {
      nextSession.claudeEffort = previousSession.claudeEffort;
      changed = true;
    }
    if (
      currentSession.geminiApprovalMode ===
        optimisticSession.geminiApprovalMode &&
      currentSession.geminiApprovalMode !== previousSession.geminiApprovalMode
    ) {
      nextSession.geminiApprovalMode = previousSession.geminiApprovalMode;
      changed = true;
    }

    return changed ? nextSession : currentSession;
  }

  function syncPreferencesFromState(nextState: StateResponse) {
    const preferences = resolveAppPreferences(nextState.preferences);
    setDefaultCodexReasoningEffort(preferences.defaultCodexReasoningEffort);
    setDefaultClaudeEffort(preferences.defaultClaudeEffort);
    setRemoteConfigs((current) =>
      areRemoteConfigsEqual(current, preferences.remotes)
        ? current
        : preferences.remotes,
    );
  }

  function adoptState(
    nextState: StateResponse,
    options?: {
      force?: boolean;
      /** Allow adopting a snapshot with a lower revision than the current one.
       *  Only used for backend restart rollbacks where the revision counter resets. */
      allowRevisionDowngrade?: boolean;
      openSessionId?: string;
      paneId?: string | null;
    },
  ) {
    if (!isMountedRef.current) {
      return false;
    }

    if (
      !shouldAdoptSnapshotRevision(
        latestStateRevisionRef.current,
        nextState.revision,
        options,
      )
    ) {
      return false;
    }

    latestStateRevisionRef.current = nextState.revision;
    const currentCodexState = codexStateRef.current;
    const currentAgentReadiness = agentReadinessRef.current;
    const currentProjects = projectsRef.current;
    const currentOrchestrators = orchestratorsRef.current;
    const currentWorkspaceSummaries = workspaceSummariesRef.current;
    const adoptedStateSlices = resolveAdoptedStateSlices(
      {
        codex: currentCodexState,
        agentReadiness: currentAgentReadiness,
        projects: currentProjects,
        orchestrators: currentOrchestrators,
        workspaces: currentWorkspaceSummaries,
      },
      nextState,
    );
    if (adoptedStateSlices.codex !== currentCodexState) {
      codexStateRef.current = adoptedStateSlices.codex;
      setCodexState(adoptedStateSlices.codex);
    }
    if (adoptedStateSlices.agentReadiness !== currentAgentReadiness) {
      agentReadinessRef.current = adoptedStateSlices.agentReadiness;
      setAgentReadiness(adoptedStateSlices.agentReadiness);
    }
    syncPreferencesFromState(nextState);
    if (adoptedStateSlices.projects !== currentProjects) {
      projectsRef.current = adoptedStateSlices.projects;
      setProjects(adoptedStateSlices.projects);
    }
    if (adoptedStateSlices.orchestrators !== currentOrchestrators) {
      orchestratorsRef.current = adoptedStateSlices.orchestrators;
      setOrchestrators(adoptedStateSlices.orchestrators);
    }
    if (adoptedStateSlices.workspaces !== currentWorkspaceSummaries) {
      workspaceSummariesRef.current = adoptedStateSlices.workspaces;
      setWorkspaceSummaries(adoptedStateSlices.workspaces);
    }
    adoptSessions(nextState.sessions, options);
    // Local state adoptions can resume or create active sessions before any SSE arrives.
    syncAdoptedLiveSessionResumeWatchdogBaselinesRef.current(
      nextState.sessions,
    );
    if (options?.openSessionId) {
      const openedSession = nextState.sessions.find(
        (session) => session.id === options.openSessionId,
      );
      setSelectedProjectId(openedSession?.projectId ?? ALL_PROJECTS_FILTER_ID);
    }
    return true;
  }

  async function persistAppPreferences(payload: {
    defaultCodexReasoningEffort?: CodexReasoningEffort;
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
    let cancelled = false;
    let initialStateResyncRetryTimeoutId: ReturnType<
      typeof window.setTimeout
    > | null = null;
    let reconnectStateResyncTimeoutId: ReturnType<
      typeof window.setTimeout
    > | null = null;
    let sawReconnectOpenSinceLastError = false;
    let nextReconnectStateResyncDelayMs = RECONNECT_STATE_RESYNC_DELAY_MS;
    let liveSessionResumeWatchdogIntervalId: ReturnType<
      typeof window.setInterval
    > | null = null;
    let shouldResyncOnResume = false;
    // These refs are component-scoped, so Strict Mode effect remounts must reset
    // any stale in-flight resync bookkeeping from the previous mount.
    stateResyncInFlightRef.current = false;
    stateResyncPendingRef.current = false;
    stateResyncAllowAuthoritativeRollbackRef.current = false;
    stateResyncPreserveReconnectFallbackRef.current = false;
    stateResyncPreserveWatchdogCooldownRef.current = false;
    stateResyncRearmOnSuccessRef.current = false;
    stateResyncRearmOnFailureRef.current = false;
    // Track transport activity per session so one noisy active session cannot
    // mask another stalled one.
    let lastLiveTransportActivityAtBySessionId = new Map<string, number>();
    // Drift-gap detection tracks a baseline per session so unrelated live traffic
    // cannot mask a wake gap for a different stalled active session.
    let lastLiveSessionResumeWatchdogTickAtBySessionId = new Map<
      string,
      number
    >();
    let lastWatchdogResyncAttemptAt: number | null = null;
    const eventSource = new EventSource("/api/events");

    function clearInitialStateResyncRetryTimeout() {
      if (initialStateResyncRetryTimeoutId === null) {
        return;
      }

      window.clearTimeout(initialStateResyncRetryTimeoutId);
      initialStateResyncRetryTimeoutId = null;
    }

    function clearReconnectStateResyncTimeout() {
      if (reconnectStateResyncTimeoutId === null) {
        return;
      }

      window.clearTimeout(reconnectStateResyncTimeoutId);
      reconnectStateResyncTimeoutId = null;
    }

    function resetReconnectStateResyncBackoff() {
      nextReconnectStateResyncDelayMs = RECONNECT_STATE_RESYNC_DELAY_MS;
    }

    function consumeReconnectStateResyncDelayMs() {
      const delayMs = nextReconnectStateResyncDelayMs;
      nextReconnectStateResyncDelayMs = Math.min(
        nextReconnectStateResyncDelayMs * 2,
        RECONNECT_STATE_RESYNC_MAX_DELAY_MS,
      );
      return delayMs;
    }

    function scheduleFallbackStateResyncRetry(
      requestedRevision: number | null,
      options: {
        allowAuthoritativeRollback?: boolean;
        preserveReconnectFallback?: boolean;
        preserveWatchdogCooldown?: boolean;
      },
    ) {
      clearInitialStateResyncRetryTimeout();
      const delayMs = consumeReconnectStateResyncDelayMs();
      initialStateResyncRetryTimeoutId = window.setTimeout(() => {
        initialStateResyncRetryTimeoutId = null;
        if (
          cancelled ||
          !readNavigatorOnline() ||
          latestStateRevisionRef.current !== requestedRevision
        ) {
          return;
        }

        requestStateResync(options);
      }, delayMs);
    }

    function clearReconnectStateResyncTimeoutAfterConfirmedReopen() {
      if (!sawReconnectOpenSinceLastError) {
        return;
      }

      clearReconnectStateResyncTimeout();
      resetReconnectStateResyncBackoff();
    }

    function confirmReconnectRecoveryFromLiveEvent() {
      if (!sawReconnectOpenSinceLastError) {
        return;
      }

      clearReconnectStateResyncTimeoutAfterConfirmedReopen();
      setBackendConnectionState("connected");
    }

    function scheduleReconnectStateResync() {
      // Preserve any reopen proof until a real EventSource.onerror starts a new
      // outage cycle. Otherwise a bad post-reopen event can arm fallback polling
      // and strand the client in "reconnecting" even when later events on the
      // same socket are healthy.
      clearReconnectStateResyncTimeout();
      const delayMs = consumeReconnectStateResyncDelayMs();
      reconnectStateResyncTimeoutId = window.setTimeout(() => {
        reconnectStateResyncTimeoutId = null;
        if (
          cancelled ||
          !readNavigatorOnline() ||
          latestStateRevisionRef.current === null
        ) {
          return;
        }

        requestStateResync({
          allowAuthoritativeRollback: true,
          rearmOnSuccess: true,
          rearmOnFailure: true,
        });
      }, delayMs);
    }

    function markLiveTransportActivity(
      sessionIds: Iterable<string>,
      now = Date.now(),
      {
        clearWatchdogCooldown = true,
      }: { clearWatchdogCooldown?: boolean } = {},
    ) {
      for (const sessionId of sessionIds) {
        lastLiveTransportActivityAtBySessionId.set(sessionId, now);
      }
      if (clearWatchdogCooldown) {
        lastWatchdogResyncAttemptAt = null;
      }
    }

    function syncLiveTransportActivityFromState(
      sessions: Session[],
      now = Date.now(),
      {
        clearWatchdogCooldown = true,
      }: { clearWatchdogCooldown?: boolean } = {},
    ) {
      // Snapshot adoption seeds the baseline for every listed session immediately.
      // Idle entries are harmless because stale-transport checks still gate on
      // session.status === "active".
      markLiveTransportActivity(
        sessions.map((session) => session.id),
        now,
        { clearWatchdogCooldown },
      );
    }

    function markLiveSessionResumeWatchdogBaseline(
      sessionIds: Iterable<string>,
      now = Date.now(),
    ) {
      for (const sessionId of sessionIds) {
        lastLiveSessionResumeWatchdogTickAtBySessionId.set(sessionId, now);
      }
    }

    function pruneLiveSessionResumeWatchdogBaselineSessions(
      sessions: Session[],
    ) {
      const liveSessionIds = new Set(sessions.map((session) => session.id));
      for (const sessionId of lastLiveSessionResumeWatchdogTickAtBySessionId.keys()) {
        if (!liveSessionIds.has(sessionId)) {
          lastLiveSessionResumeWatchdogTickAtBySessionId.delete(sessionId);
        }
      }
    }

    function syncLiveSessionResumeWatchdogBaselines(
      sessions: Session[],
      now = Date.now(),
    ) {
      // Advance every currently known session so idle-to-active transitions do not
      // inherit a false wake gap from time spent without live streaming.
      markLiveSessionResumeWatchdogBaseline(
        sessions.map((session) => session.id),
        now,
      );
      pruneLiveSessionResumeWatchdogBaselineSessions(sessions);
    }
    syncAdoptedLiveSessionResumeWatchdogBaselinesRef.current =
      syncLiveSessionResumeWatchdogBaselines;

    function startStateResyncLoop() {
      if (
        cancelled ||
        stateResyncInFlightRef.current ||
        !stateResyncPendingRef.current
      ) {
        return;
      }

      stateResyncInFlightRef.current = true;
      void (async () => {
        try {
          while (!cancelled && stateResyncPendingRef.current) {
            stateResyncPendingRef.current = false;
            const allowAuthoritativeRollback =
              stateResyncAllowAuthoritativeRollbackRef.current;
            stateResyncAllowAuthoritativeRollbackRef.current = false;
            const preserveReconnectFallback =
              stateResyncPreserveReconnectFallbackRef.current;
            stateResyncPreserveReconnectFallbackRef.current = false;
            const preserveWatchdogCooldown =
              stateResyncPreserveWatchdogCooldownRef.current;
            stateResyncPreserveWatchdogCooldownRef.current = false;
            const rearmOnSuccess =
              stateResyncRearmOnSuccessRef.current;
            stateResyncRearmOnSuccessRef.current = false;
            const rearmOnFailure =
              stateResyncRearmOnFailureRef.current;
            stateResyncRearmOnFailureRef.current = false;
            const requestedRevision = latestStateRevisionRef.current;

            try {
              const state = await fetchState();
              if (cancelled) {
                break;
              }

              const shouldForceRollback =
                allowAuthoritativeRollback &&
                requestedRevision !== null &&
                latestStateRevisionRef.current === requestedRevision &&
                state.revision <= requestedRevision;

              const adopted = adoptState(state, {
                // A reconnect fallback snapshot is authoritative if no newer SSE state landed
                // while it was in flight, even when a crashed backend restarted below the last
                // streamed client revision.
                force: shouldForceRollback,
                allowRevisionDowngrade: shouldForceRollback,
              });
              if (adopted) {
                clearInitialStateResyncRetryTimeout();
                if (
                  reconnectStateResyncTimeoutId !== null &&
                  sawReconnectOpenSinceLastError
                ) {
                  // Once the stream itself has reopened, an adopted snapshot can
                  // disarm any leftover reconnect fallback timer and reset the
                  // next reconnect cycle back to the fast initial delay.
                  clearReconnectStateResyncTimeout();
                  resetReconnectStateResyncBackoff();
                }
                const adoptedAt = Date.now();
                syncLiveTransportActivityFromState(state.sessions, adoptedAt, {
                  clearWatchdogCooldown: !preserveWatchdogCooldown,
                });
                pruneLiveTransportActivitySessions(
                  lastLiveTransportActivityAtBySessionId,
                  state.sessions,
                );
                syncLiveSessionResumeWatchdogBaselines(
                  state.sessions,
                  adoptedAt,
                );
              }
              setBackendConnectionIssueDetail(null);
              clearRecoveredBackendRequestError();
              if (
                rearmOnSuccess &&
                requestedRevision !== null &&
                latestStateRevisionRef.current === requestedRevision &&
                !sawReconnectOpenSinceLastError &&
                reconnectStateResyncTimeoutId === null
              ) {
                // Reconnect fallback probes must keep polling authoritative
                // state until the live SSE transport proves it reopened.
                // Generic watchdog or wake-gap resyncs stay one-shot.
                scheduleReconnectStateResync();
              }
            } catch (error) {
              if (!cancelled) {
                setBackendConnectionIssueDetail(
                  describeBackendConnectionIssueDetail(error),
                );
                const errorRequiresRestart =
                  isBackendUnavailableError(error) && error.restartRequired;
                if (errorRequiresRestart) {
                  // Incompatible backend serving HTML — retrying will produce
                  // the same result until the user restarts.
                } else if (
                  preserveReconnectFallback &&
                  reconnectStateResyncTimeoutId === null
                ) {
                  // Marked fallback snapshots can arrive without a preceding reconnect
                  // onerror, so transient /api/state failures need their own retry.
                  scheduleFallbackStateResyncRetry(requestedRevision, {
                    allowAuthoritativeRollback,
                    preserveReconnectFallback,
                    preserveWatchdogCooldown,
                  });
                } else if (
                  rearmOnFailure &&
                  reconnectStateResyncTimeoutId === null &&
                  requestedRevision !== null
                ) {
                  // Re-arm reconnect polling so a failed one-shot probe (e.g.
                  // manual retry) does not leave the client without any automatic
                  // recovery path until the next EventSource onerror fires.
                  scheduleReconnectStateResync();
                }
              }
              break;
            } finally {
              if (!cancelled) {
                setIsLoading(false);
              }
            }
          }
        } finally {
          if (!cancelled) {
            // Clear before restarting so startStateResyncLoop's entry guard passes.
            stateResyncInFlightRef.current = false;
            if (stateResyncPendingRef.current) {
              startStateResyncLoop();
            }
          }
        }
      })();
    }

    function requestStateResync(options?: {
      allowAuthoritativeRollback?: boolean;
      preserveReconnectFallback?: boolean;
      preserveWatchdogCooldown?: boolean;
      rearmOnSuccess?: boolean;
      rearmOnFailure?: boolean;
    }) {
      if (cancelled) {
        return;
      }

      if (
        sawReconnectOpenSinceLastError &&
        !options?.preserveReconnectFallback
      ) {
        clearReconnectStateResyncTimeout();
      }
      // Otherwise preserve the armed reconnect fallback; startStateResyncLoop()
      // will only disarm it once a /api/state snapshot is actually adopted.
      // Coalesced resync requests retain the strongest semantics until the next
      // loop iteration consumes them.
      if (options?.allowAuthoritativeRollback) {
        stateResyncAllowAuthoritativeRollbackRef.current = true;
      }
      if (options?.preserveReconnectFallback) {
        stateResyncPreserveReconnectFallbackRef.current = true;
      }
      if (options?.preserveWatchdogCooldown) {
        stateResyncPreserveWatchdogCooldownRef.current = true;
      }
      if (options?.rearmOnSuccess) {
        stateResyncRearmOnSuccessRef.current = true;
      }
      if (options?.rearmOnFailure) {
        stateResyncRearmOnFailureRef.current = true;
      }
      stateResyncPendingRef.current = true;
      startStateResyncLoop();
    }
    requestBackendReconnectRef.current = () => {
      if (cancelled || !readNavigatorOnline()) {
        return;
      }

      sawReconnectOpenSinceLastError = false;
      clearInitialStateResyncRetryTimeout();
      clearReconnectStateResyncTimeout();
      resetReconnectStateResyncBackoff();
      // Manual retry is intentionally an immediate `/api/state` probe. It does
      // not preserve any currently armed reconnect timer, but success or
      // failure both hand control back to the normal reconnect cycle.
      requestStateResync({
        allowAuthoritativeRollback: latestStateRevisionRef.current !== null,
        rearmOnSuccess: true,
        rearmOnFailure: true,
      });
    };
    requestActionRecoveryResyncRef.current = () => {
      if (cancelled || !readNavigatorOnline()) {
        return;
      }

      // Action-error recovery is a plain one-shot `/api/state` probe. Unlike
      // the manual retry path it must NOT reset `sawReconnectOpenSinceLastError`
      // or re-arm reconnect polling, because the SSE stream may still be
      // healthy — only the individual request endpoint returned a transient
      // backend-unavailable error (e.g. a one-off 502). Touching SSE-level
      // state here would cause the successful probe to schedule reconnect
      // polling that never disarms until an unrelated EventSource onerror/onopen
      // cycle resets the flag.
      requestStateResync({
        allowAuthoritativeRollback: latestStateRevisionRef.current !== null,
      });
    };

    function hasPotentiallyStaleLiveSession() {
      return sessionsRef.current.some((session) => session.status !== "idle");
    }

    function hasActivelyStreamingSession() {
      return sessionsRef.current.some((session) => session.status === "active");
    }

    function hasPotentiallyStaleTransportSession(now: number) {
      return sessionsRef.current.some((session) =>
        sessionHasPotentiallyStaleTransport(
          session,
          lastLiveTransportActivityAtBySessionId.get(session.id),
          now,
        ),
      );
    }

    function flushWorkspaceFilesChangedEventBuffer() {
      workspaceFilesChangedEventFlushTimeoutRef.current = null;
      const bufferedEvent = workspaceFilesChangedEventBufferRef.current;
      workspaceFilesChangedEventBufferRef.current = null;
      if (!bufferedEvent || cancelled) {
        return;
      }

      startTransition(() => {
        setWorkspaceFilesChangedEvent(bufferedEvent);
      });
    }

    function resetWorkspaceFilesChangedEventGate() {
      lastWorkspaceFilesChangedRevisionRef.current = null;
      workspaceFilesChangedEventBufferRef.current = null;
      if (workspaceFilesChangedEventFlushTimeoutRef.current !== null) {
        window.clearTimeout(workspaceFilesChangedEventFlushTimeoutRef.current);
        workspaceFilesChangedEventFlushTimeoutRef.current = null;
      }
    }

    function enqueueWorkspaceFilesChangedEvent(
      filesChanged: WorkspaceFilesChangedEvent,
    ) {
      const lastRevision = lastWorkspaceFilesChangedRevisionRef.current;
      if (lastRevision !== null && filesChanged.revision < lastRevision) {
        return;
      }

      lastWorkspaceFilesChangedRevisionRef.current = filesChanged.revision;
      workspaceFilesChangedEventBufferRef.current =
        mergeWorkspaceFilesChangedEvents(
          workspaceFilesChangedEventBufferRef.current,
          filesChanged,
        );

      if (workspaceFilesChangedEventFlushTimeoutRef.current !== null) {
        return;
      }

      workspaceFilesChangedEventFlushTimeoutRef.current = window.setTimeout(
        flushWorkspaceFilesChangedEventBuffer,
        0,
      );
    }

    function markResumeResyncIfNeeded() {
      if (latestStateRevisionRef.current === null) {
        return;
      }

      shouldResyncOnResume = hasPotentiallyStaleLiveSession();
    }

    function resumeStateIfNeeded() {
      if (
        cancelled ||
        !shouldResyncOnResume ||
        !readNavigatorOnline() ||
        latestStateRevisionRef.current === null
      ) {
        return;
      }

      shouldResyncOnResume = false;
      requestStateResync({ allowAuthoritativeRollback: true });
    }

    function handleLiveSessionResumeWatchdogTick() {
      const now = Date.now();
      if (cancelled) {
        return;
      }

      if (document.visibilityState === "hidden") {
        return;
      }

      if (
        !readNavigatorOnline() ||
        stateResyncInFlightRef.current ||
        stateResyncPendingRef.current
      ) {
        return;
      }

      const detectedResumeGap = sessionsRef.current.some((session) => {
        if (session.status !== "active") {
          return false;
        }

        const lastWatchdogTickAt =
          lastLiveSessionResumeWatchdogTickAtBySessionId.get(session.id) ?? now;
        return (
          now - lastWatchdogTickAt >= LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS
        );
      });
      // Only advance baselines when this tick is actually evaluating drift.
      // Pausing during in-flight/pending resyncs avoids consuming a real wake gap.
      syncLiveSessionResumeWatchdogBaselines(sessionsRef.current, now);
      if (
        latestStateRevisionRef.current === null ||
        !hasActivelyStreamingSession()
      ) {
        return;
      }

      // Wake-gap recovery stays broader than the stale-transport path so a first
      // assistant reply that finished while the machine was asleep can still recover,
      // even when queued follow-ups exist behind the active turn.
      const transportLooksStale = hasPotentiallyStaleTransportSession(now);
      if (!detectedResumeGap && !transportLooksStale) {
        return;
      }

      const watchdogRetryCooldownElapsed =
        lastWatchdogResyncAttemptAt === null ||
        now - lastWatchdogResyncAttemptAt >=
          LIVE_SESSION_WATCHDOG_RESYNC_RETRY_COOLDOWN_MS;
      if (!watchdogRetryCooldownElapsed) {
        return;
      }

      // A focused window can wake without blur/focus or visibility transitions.
      lastWatchdogResyncAttemptAt = now;
      requestStateResync({
        allowAuthoritativeRollback: true,
        preserveWatchdogCooldown: true,
      });
    }

    function handleStateEvent(event: MessageEvent<string>) {
      if (cancelled) {
        return;
      }

      try {
        const state = JSON.parse(event.data) as StateEventPayload;
        if (state._sseFallback) {
          // Marked fallback payloads only signal that the client should refetch
          // the authoritative snapshot from /api/state.
          forceAdoptNextStateEventRef.current = false;
          requestStateResync({
            allowAuthoritativeRollback: true,
            preserveReconnectFallback: true,
          });
          return;
        }

        const force = forceAdoptNextStateEventRef.current;
        // SSE state events are always the first event on a new connection
        // (before any deltas), so there is no risk of a delta racing ahead
        // and being overwritten. Allow revision downgrade so a restarted
        // server (whose persisted revision may be lower) is adopted.
        const adopted = adoptState(state, {
          force,
          allowRevisionDowngrade: force,
        });
        forceAdoptNextStateEventRef.current = false;
        // Confirm recovery only after adoption succeeds. If adoptState throws
        // (bad payload, reducer error), the catch block must keep the client in
        // the reconnecting state with fallback polling armed rather than
        // prematurely marking the connection as healthy.
        confirmReconnectRecoveryFromLiveEvent();
        if (adopted) {
          clearInitialStateResyncRetryTimeout();
          const adoptedAt = Date.now();
          clearReconnectStateResyncTimeoutAfterConfirmedReopen();
          // A live SSE state payload proves the stream is healthy again, so it also
          // clears any residual watchdog retry cooldown from an earlier fallback probe.
          syncLiveTransportActivityFromState(state.sessions, adoptedAt);
          pruneLiveTransportActivitySessions(
            lastLiveTransportActivityAtBySessionId,
            state.sessions,
          );
          syncLiveSessionResumeWatchdogBaselines(state.sessions, adoptedAt);
        }
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
      } catch (error) {
        if (!cancelled) {
          setBackendConnectionIssueDetail(
            describeBackendConnectionIssueDetail(error),
          );
          // A bad reconnect state payload must not leave the client marked as
          // connected without a usable snapshot. Restore "reconnecting" so the
          // retry affordance stays available (onopen already set "connected"),
          // and re-arm fallback polling so recovery continues via /api/state.
          if (sawReconnectOpenSinceLastError) {
            setBackendConnectionState("reconnecting");
            if (reconnectStateResyncTimeoutId === null) {
              scheduleReconnectStateResync();
            }
          }
        }
      } finally {
        if (!cancelled) {
          setIsLoading(false);
        }
      }
    }

    function handleDeltaEvent(event: MessageEvent<string>) {
      if (cancelled) {
        return;
      }

      try {
        const delta = JSON.parse(event.data) as DeltaEvent;
        const revisionAction = decideDeltaRevisionAction(
          latestStateRevisionRef.current,
          delta.revision,
        );
        if (revisionAction === "ignore") {
          // An ignored delta proves the client already has data at this
          // revision or newer — the snapshot that advanced the revision was
          // authoritative. Transport is healthy and the client is caught up.
          confirmReconnectRecoveryFromLiveEvent();
          clearReconnectStateResyncTimeoutAfterConfirmedReopen();
          setBackendConnectionIssueDetail(null);
          clearRecoveredBackendRequestError();
          return;
        }
        if (revisionAction === "resync") {
          // A revision gap means we missed events but the stream IS working.
          // Do NOT confirm recovery yet — if the follow-up /api/state fetch
          // fails, the client must stay in the reconnecting state. Use
          // rearmOnFailure so a failed resync re-arms polling instead of
          // stalling recovery.
          requestStateResync({ rearmOnFailure: true });
          return;
        }

        if (delta.type === "orchestratorsUpdated") {
          // Global orchestrator updates prove the SSE stream is healthy enough to
          // clear reconnect fallback state. When the delta also carries session
          // snapshots, treat those specific ids as live data for watchdog baselines.
          confirmReconnectRecoveryFromLiveEvent();
          clearReconnectStateResyncTimeoutAfterConfirmedReopen();
          const appliedAt = Date.now();
          if (delta.sessions?.length) {
            const deltaSessionIds = delta.sessions.map((session) => session.id);
            markLiveTransportActivity(deltaSessionIds, appliedAt);
            markLiveSessionResumeWatchdogBaseline(deltaSessionIds, appliedAt);
          }
          latestStateRevisionRef.current = delta.revision;
          const nextSessions = mergeOrchestratorDeltaSessions(
            sessionsRef.current,
            delta.sessions,
          );
          sessionsRef.current = nextSessions;
          orchestratorsRef.current = delta.orchestrators;
          startTransition(() => {
            setOrchestrators(delta.orchestrators);
            setSessions(nextSessions);
          });
          setBackendConnectionIssueDetail(null);
          clearRecoveredBackendRequestError();
          return;
        }

        // Non-session deltas such as orchestratorsUpdated are handled above; the
        // session reducer only accepts deltas that carry a concrete sessionId.
        const result = applyDeltaToSessions(sessionsRef.current, delta);
        if (result.kind === "applied") {
          confirmReconnectRecoveryFromLiveEvent();
          const appliedAt = Date.now();
          clearReconnectStateResyncTimeoutAfterConfirmedReopen();
          // Every session-scoped delta proves liveness for that session, including
          // any future delta shape that revives it back to "active".
          markLiveTransportActivity([delta.sessionId], appliedAt);
          markLiveSessionResumeWatchdogBaseline([delta.sessionId], appliedAt);
          latestStateRevisionRef.current = delta.revision;
          sessionsRef.current = result.sessions;
          startTransition(() => {
            setSessions(sessionsRef.current);
          });
          setBackendConnectionIssueDetail(null);
          clearRecoveredBackendRequestError();
          return;
        }

        // Unrecognized delta type — resync to get an authoritative snapshot.
        requestStateResync({ rearmOnFailure: true });
      } catch {
        // Parse or reducer failure — restore reconnecting state so the retry
        // affordance stays available, and re-arm polling.
        if (sawReconnectOpenSinceLastError) {
          setBackendConnectionState("reconnecting");
          if (reconnectStateResyncTimeoutId === null) {
            scheduleReconnectStateResync();
          }
        } else {
          requestStateResync({ rearmOnFailure: true });
        }
      }
    }

    function handleWorkspaceFilesChangedEvent(event: MessageEvent<string>) {
      if (cancelled) {
        return;
      }

      try {
        const filesChanged = JSON.parse(event.data) as WorkspaceFilesChangedEvent;
        confirmReconnectRecoveryFromLiveEvent();
        clearReconnectStateResyncTimeoutAfterConfirmedReopen();
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        enqueueWorkspaceFilesChangedEvent(filesChanged);
      } catch {
        // File-change events are non-authoritative hints. If one is malformed,
        // keep the main state stream alive and wait for the next event/snapshot.
      }
    }

    eventSource.addEventListener("state", handleStateEvent as EventListener);
    eventSource.addEventListener("delta", handleDeltaEvent as EventListener);
    eventSource.addEventListener(
      "workspaceFilesChanged",
      handleWorkspaceFilesChangedEvent as EventListener,
    );
    eventSource.onopen = () => {
      if (!cancelled) {
        resetWorkspaceFilesChangedEventGate();
        sawReconnectOpenSinceLastError = true;
        if (latestStateRevisionRef.current !== null) {
          // A restarted backend can reconnect with the same persisted revision but a
          // more complete authoritative snapshot than the client currently has.
          forceAdoptNextStateEventRef.current = true;
        }
        setBackendConnectionState("connected");
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
      }
    };
    eventSource.onerror = () => {
      if (cancelled) {
        return;
      }

      const hadReconnectOpenSinceLastError = sawReconnectOpenSinceLastError;
      sawReconnectOpenSinceLastError = false;
      const isOnline = readNavigatorOnline();
      const hasHydratedState = latestStateRevisionRef.current !== null;
      setBackendConnectionState(
        isOnline
          ? hasHydratedState
            ? "reconnecting"
            : "connecting"
          : "offline",
      );
      if (!isOnline) {
        clearInitialStateResyncRetryTimeout();
        clearReconnectStateResyncTimeout();
        resetReconnectStateResyncBackoff();
        return;
      }

      if (!hasHydratedState) {
        requestStateResync();
        return;
      }

      // Prefer the SSE reconnect snapshot when it arrives quickly, but fall back to /api/state
      // so completed assistant replies do not stay hidden until another user action forces a refresh.
      if (hadReconnectOpenSinceLastError) {
        // The stream had reopened since the previous error, so this is a new
        // failure cycle. Discard any stale timer and start fresh at the fast
        // initial delay.
        clearReconnectStateResyncTimeout();
        resetReconnectStateResyncBackoff();
        scheduleReconnectStateResync();
      } else if (reconnectStateResyncTimeoutId === null) {
        // Repeated error during the same outage — only schedule if no
        // fallback timer is already pending so the exponential backoff
        // is preserved.
        scheduleReconnectStateResync();
      }
    };
    liveSessionResumeWatchdogIntervalId = window.setInterval(
      handleLiveSessionResumeWatchdogTick,
      LIVE_SESSION_RESUME_WATCHDOG_INTERVAL_MS,
    );

    function handleWindowBlur() {
      markResumeResyncIfNeeded();
    }

    function handlePageHide() {
      markResumeResyncIfNeeded();
    }

    function handlePageShow() {
      resumeStateIfNeeded();
    }

    function handleWindowFocus() {
      if (document.visibilityState === "hidden") {
        return;
      }

      resumeStateIfNeeded();
    }

    function handleVisibilityChange() {
      if (document.visibilityState === "hidden") {
        markResumeResyncIfNeeded();
        return;
      }

      resumeStateIfNeeded();
    }

    window.addEventListener("blur", handleWindowBlur);
    window.addEventListener("focus", handleWindowFocus);
    window.addEventListener("pagehide", handlePageHide);
    window.addEventListener("pageshow", handlePageShow);
    document.addEventListener("visibilitychange", handleVisibilityChange);

    return () => {
      cancelled = true;
      requestBackendReconnectRef.current = () => {};
      requestActionRecoveryResyncRef.current = () => {};
      syncAdoptedLiveSessionResumeWatchdogBaselinesRef.current = () => {};
      clearInitialStateResyncRetryTimeout();
      clearReconnectStateResyncTimeout();
      if (workspaceFilesChangedEventFlushTimeoutRef.current !== null) {
        window.clearTimeout(workspaceFilesChangedEventFlushTimeoutRef.current);
        workspaceFilesChangedEventFlushTimeoutRef.current = null;
      }
      workspaceFilesChangedEventBufferRef.current = null;
      if (liveSessionResumeWatchdogIntervalId !== null) {
        window.clearInterval(liveSessionResumeWatchdogIntervalId);
      }
      window.removeEventListener("blur", handleWindowBlur);
      window.removeEventListener("focus", handleWindowFocus);
      window.removeEventListener("pagehide", handlePageHide);
      window.removeEventListener("pageshow", handlePageShow);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      eventSource.removeEventListener(
        "state",
        handleStateEvent as EventListener,
      );
      eventSource.removeEventListener(
        "delta",
        handleDeltaEvent as EventListener,
      );
      eventSource.removeEventListener(
        "workspaceFilesChanged",
        handleWorkspaceFilesChangedEvent as EventListener,
      );
      eventSource.close();
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

  useLayoutEffect(() => {
    applyThemePreference(themeId);
    // Also update the global fallback key so main.tsx can use it for new workspaces
    persistThemePreference(themeId);
  }, [themeId]);

  useLayoutEffect(() => {
    applyStylePreference(styleId);
    persistStylePreference(styleId);
  }, [styleId]);

  useLayoutEffect(() => {
    applyFontSizePreference(fontSizePx);
    persistFontSizePreference(fontSizePx);
  }, [fontSizePx]);

  useLayoutEffect(() => {
    applyDensityPreference(densityPercent);
    persistDensityPreference(densityPercent);
  }, [densityPercent]);

  useEffect(() => {
    persistEditorFontSizePreference(editorFontSizePx);
  }, [editorFontSizePx]);

  useEffect(() => {
    let cancelled = false;
    workspaceLayoutLoadPendingRef.current = true;
    ignoreFetchedWorkspaceLayoutRef.current = false;
    setIsWorkspaceLayoutReady(false);

    void fetchWorkspaceLayout(workspaceViewId)
      .then((response) => {
        if (cancelled) {
          return;
        }

        const nextLayout = response
          ? parseStoredWorkspaceLayout(
              JSON.stringify({
                controlPanelSide: response.layout.controlPanelSide,
                themeId: response.layout.themeId,
                styleId: response.layout.styleId,
                fontSizePx: response.layout.fontSizePx,
                editorFontSizePx: response.layout.editorFontSizePx,
                densityPercent: response.layout.densityPercent,
                workspace: response.layout.workspace,
              }),
            )
          : null;

        if (nextLayout) {
          const shouldApplyFetchedWorkspaceLayout =
            !ignoreFetchedWorkspaceLayoutRef.current;
          // A manual layout change during hydration claims the workspace tree
          // and dock side locally, but still allows the server-stored visual
          // preferences to merge in once the fetch resolves.
          if (shouldApplyFetchedWorkspaceLayout) {
            setControlPanelSide(nextLayout.controlPanelSide);
          }
          if (nextLayout.themeId) {
            setThemeId(nextLayout.themeId);
          }
          if (nextLayout.styleId) {
            setStyleId(nextLayout.styleId);
          }
          if (nextLayout.fontSizePx !== undefined) {
            setFontSizePx(nextLayout.fontSizePx);
          }
          if (nextLayout.editorFontSizePx !== undefined) {
            setEditorFontSizePx(nextLayout.editorFontSizePx);
          }
          if (nextLayout.densityPercent !== undefined) {
            setDensityPercent(nextLayout.densityPercent);
          }
          if (shouldApplyFetchedWorkspaceLayout) {
            setWorkspace(
              hydrateControlPanelLayout(
                nextLayout.workspace,
                nextLayout.controlPanelSide,
              ),
            );
            persistWorkspaceLayout(workspaceViewId, nextLayout);
          }
        }

        // A successful layout fetch proves the route that restart-required
        // errors report as broken is now functional. Clear the stale toast
        // only if the current requestError is the exact message we set.
        const staleRestartMessage =
          workspaceLayoutRestartErrorMessageRef.current;
        if (staleRestartMessage !== null) {
          workspaceLayoutRestartErrorMessageRef.current = null;
          setRequestError((current) =>
            resolveRecoveredWorkspaceLayoutRequestError(
              current,
              staleRestartMessage,
            ),
          );
        }
        workspaceLayoutLoadPendingRef.current = false;
        setIsWorkspaceLayoutReady(true);
      })
      .catch((error) => {
        console.warn(
          "workspace layout warning> failed to load server workspace layout:",
          error,
        );
        if (!cancelled) {
          // Restart-required errors indicate an incompatible backend; surface
          // the restart instruction to the user instead of silently degrading.
          if (isBackendUnavailableError(error) && error.restartRequired) {
            const message = getErrorMessage(error);
            workspaceLayoutRestartErrorMessageRef.current = message;
            reportRequestError(error);
          }
          workspaceLayoutLoadPendingRef.current = false;
          setIsWorkspaceLayoutReady(true);
        }
      });

    return () => {
      cancelled = true;
      workspaceLayoutLoadPendingRef.current = false;
    };
  }, [workspaceViewId]);

  useEffect(() => {
    if (!isWorkspaceLayoutReady) {
      return;
    }

    const persistedWorkspace = stripLoadingGitDiffPreviewTabsFromWorkspaceState(
      applyControlPanelLayout(workspace, controlPanelSide),
    );
    const layout: WorkspaceLayoutPersistencePayload = {
      controlPanelSide,
      themeId,
      styleId,
      fontSizePx,
      editorFontSizePx,
      densityPercent,
      workspace: persistedWorkspace,
    };
    persistWorkspaceLayout(workspaceViewId, layout);
    pendingWorkspaceLayoutSaveRef.current = {
      workspaceId: workspaceViewId,
      layout,
    };

    clearPendingWorkspaceLayoutSaveTimeout();
    const persistTimeout = window.setTimeout(() => {
      flushPendingWorkspaceLayoutSave();
    }, WORKSPACE_LAYOUT_PERSIST_DELAY_MS);
    pendingWorkspaceLayoutSaveTimeoutRef.current = persistTimeout;

    return () => {
      if (pendingWorkspaceLayoutSaveTimeoutRef.current === persistTimeout) {
        clearPendingWorkspaceLayoutSaveTimeout();
      }
    };
  }, [
    controlPanelSide,
    densityPercent,
    editorFontSizePx,
    fontSizePx,
    isWorkspaceLayoutReady,
    styleId,
    themeId,
    workspace,
    workspaceViewId,
  ]);

  useEffect(() => {
    function handlePageHide() {
      flushWorkspaceLayoutSaveRef.current({ keepalive: true });
    }

    window.addEventListener("pagehide", handlePageHide);
    return () => {
      window.removeEventListener("pagehide", handlePageHide);
    };
  }, []);

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

  useEffect(() => {
    codexStateRef.current = codexState;
    agentReadinessRef.current = agentReadiness;
    projectsRef.current = projects;
    orchestratorsRef.current = orchestrators;
    workspaceSummariesRef.current = workspaceSummaries;
  }, [
    agentReadiness,
    codexState,
    orchestrators,
    projects,
    workspaceSummaries,
  ]);

  useEffect(() => {
    draftAttachmentsRef.current = draftAttachmentsBySessionId;
  }, [draftAttachmentsBySessionId]);

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
        (gitDiffPreviewRefreshVersionsRef.current.get(refresh.requestKey) ?? 0) + 1;
      gitDiffPreviewRefreshVersionsRef.current.set(refresh.requestKey, currentVersion);

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
    if (typeof BroadcastChannel === "undefined") {
      return;
    }

    const channel = new BroadcastChannel(TAB_DRAG_CHANNEL_NAME);
    dragChannelRef.current = channel;
    channel.onmessage = (event: MessageEvent<unknown>) => {
      const message = event.data;
      if (!isWorkspaceTabDragChannelMessage(message)) {
        return;
      }

      switch (message.type) {
        case "drag-start":
          if (message.payload.sourceWindowId !== windowId) {
            setExternalDraggedTab(message.payload);
          }
          break;
        case "drag-end":
          setExternalDraggedTab((current) =>
            current?.dragId === message.dragId ? null : current,
          );
          break;
        case "drop-commit":
          if (message.sourceWindowId !== windowId) {
            break;
          }

          if (draggedTabRef.current?.dragId === message.dragId) {
            draggedTabRef.current = null;
          }
          setDraggedTab((current) =>
            current?.dragId === message.dragId ? null : current,
          );
          setWorkspace((current) =>
            applyControlPanelLayout(
              closeWorkspaceTab(current, message.sourcePaneId, message.tabId),
            ),
          );
          break;
      }
    };

    return () => {
      channel.close();
      if (dragChannelRef.current === channel) {
        dragChannelRef.current = null;
      }
    };
  }, [windowId]);

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
      if (activePromptPollIntervalRef.current !== null) {
        clearInterval(activePromptPollIntervalRef.current);
        activePromptPollIntervalRef.current = null;
      }
      if (activePromptPollTimeoutRef.current !== null) {
        clearTimeout(activePromptPollTimeoutRef.current);
        activePromptPollTimeoutRef.current = null;
      }
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
    return () => {
      clearPendingKillCloseTimeout();
      clearPendingSessionRenameCloseTimeout();
    };
  }, []);

  useEffect(() => {
    if (!pendingKillSessionId) {
      clearPendingKillCloseTimeout();
      return;
    }

    const focusFrameId = window.requestAnimationFrame(() => {
      pendingKillConfirmButtonRef.current?.focus();
    });

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        closePendingKillConfirmation(true);
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.cancelAnimationFrame(focusFrameId);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [pendingKillSessionId]);

  useLayoutEffect(() => {
    if (!pendingKillSessionId) {
      setPendingKillPopoverStyle(null);
      return;
    }

    setPendingKillPopoverStyle({
      left: 0,
      top: 0,
      visibility: "hidden",
    });

    function updatePendingKillPopoverStyle() {
      const trigger = pendingKillTriggerRef.current;
      const popover = pendingKillPopoverRef.current;
      if (!trigger || !popover) {
        return;
      }

      const triggerRect = trigger.getBoundingClientRect();
      const popoverRect = popover.getBoundingClientRect();
      const viewportPadding = 12;
      const preferredLeft =
        triggerRect.left + triggerRect.width / 2 - popoverRect.width / 2;
      const left = clamp(
        preferredLeft,
        viewportPadding,
        window.innerWidth - popoverRect.width - viewportPadding,
      );
      const preferredTop = triggerRect.top - 10;
      const top = clamp(
        preferredTop,
        viewportPadding,
        window.innerHeight - popoverRect.height - viewportPadding,
      );

      setPendingKillPopoverStyle({
        left,
        top,
      });
    }

    const frameId = window.requestAnimationFrame(updatePendingKillPopoverStyle);
    window.addEventListener("resize", updatePendingKillPopoverStyle);
    window.addEventListener("scroll", updatePendingKillPopoverStyle, true);

    return () => {
      window.cancelAnimationFrame(frameId);
      window.removeEventListener("resize", updatePendingKillPopoverStyle);
      window.removeEventListener("scroll", updatePendingKillPopoverStyle, true);
    };
  }, [pendingKillSessionId]);

  useEffect(() => {
    if (!pendingSessionRename) {
      clearPendingSessionRenameCloseTimeout();
      return;
    }

    const focusFrameId = window.requestAnimationFrame(() => {
      pendingSessionRenameInputRef.current?.focus();
      pendingSessionRenameInputRef.current?.select();
    });

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        closePendingSessionRename(true);
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.cancelAnimationFrame(focusFrameId);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [pendingSessionRename]);

  useLayoutEffect(() => {
    if (!pendingSessionRename) {
      setPendingSessionRenameStyle(null);
      return;
    }

    const renameAnchor = pendingSessionRename;

    setPendingSessionRenameStyle({
      left: 0,
      top: 0,
      visibility: "hidden",
    });

    function updatePendingSessionRenameStyle() {
      const popover = pendingSessionRenamePopoverRef.current;
      if (!popover) {
        return;
      }

      const popoverRect = popover.getBoundingClientRect();
      const viewportPadding = 12;
      const left = clamp(
        renameAnchor.clientX - popoverRect.width / 2,
        viewportPadding,
        window.innerWidth - popoverRect.width - viewportPadding,
      );
      const top = clamp(
        renameAnchor.clientY - 18,
        viewportPadding,
        window.innerHeight - popoverRect.height - viewportPadding,
      );

      setPendingSessionRenameStyle({
        left,
        top,
      });
    }

    const frameId = window.requestAnimationFrame(
      updatePendingSessionRenameStyle,
    );
    window.addEventListener("resize", updatePendingSessionRenameStyle);

    return () => {
      window.cancelAnimationFrame(frameId);
      window.removeEventListener("resize", updatePendingSessionRenameStyle);
    };
  }, [pendingSessionRename]);

  useEffect(() => {
    if (!isSettingsOpen) {
      return;
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        setIsSettingsOpen(false);
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [isSettingsOpen]);

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


  useEffect(() => {
    function handlePointerMove(event: PointerEvent) {
      const resizeState = resizeStateRef.current;
      if (!resizeState) {
        return;
      }

      const delta =
        resizeState.direction === "row"
          ? event.clientX - resizeState.startX
          : event.clientY - resizeState.startY;
      const nextRatio = clamp(
        resizeState.startRatio + delta / Math.max(resizeState.size, 1),
        resizeState.minRatio,
        resizeState.maxRatio,
      );
      if (
        workspaceLayoutLoadPendingRef.current &&
        nextRatio !== resizeState.startRatio
      ) {
        // Keep a manual resize from being overwritten by a late initial layout
        // fetch for the current workspace.
        ignoreFetchedWorkspaceLayoutRef.current = true;
      }

      setWorkspace((current) =>
        updateSplitRatio(current, resizeState.splitId, nextRatio),
      );
    }

    function handlePointerUp() {
      resizeStateRef.current = null;
    }

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);

    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
    };
  }, []);

  function handleSend(
    sessionId: string,
    draftTextOverride?: string,
    expandedTextOverride?: string | null,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return false;
    }

    const draftText = draftTextOverride ?? draftsBySessionId[sessionId] ?? "";
    const prompt = draftText.trim();
    const expandedText = expandedTextOverride?.trim() || null;
    const normalizedExpandedText =
      expandedText && expandedText !== prompt ? expandedText : null;
    const attachments = draftAttachmentsBySessionId[sessionId] ?? [];
    if (!prompt && attachments.length === 0) {
      return false;
    }
    if (
      session.agent === "Codex" &&
      session.externalSessionId &&
      session.codexThreadState === "archived"
    ) {
      setRequestError(
        "This Codex thread is archived. Unarchive it before sending another prompt.",
      );
      return false;
    }
    const unknownModelAttempt = resolveUnknownSessionModelSendAttempt(
      confirmedUnknownModelSendsRef.current,
      session,
    );
    confirmedUnknownModelSendsRef.current =
      unknownModelAttempt.nextConfirmedKeys;
    if (!unknownModelAttempt.allowSend) {
      setRequestError(unknownModelAttempt.warning);
      return false;
    }

    setSendingSessionIds((current) => setSessionFlag(current, sessionId, true));
    setDraftsBySessionId((current) => {
      if ((current[sessionId] ?? "") === "") {
        return current;
      }

      return {
        ...current,
        [sessionId]: "",
      };
    });
    setDraftAttachmentsBySessionId((current) => {
      if (!current[sessionId]?.length) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[sessionId];
      return nextState;
    });

    void (async () => {
      try {
        const state = await sendMessage(
          sessionId,
          prompt,
          attachments.map((attachment) => ({
            data: attachment.base64Data,
            fileName: attachment.fileName,
            mediaType: attachment.mediaType,
          })),
          normalizedExpandedText,
        );
        adoptState(state);
        releaseDraftAttachments(attachments);
        setRequestError(null);
        // Safety net: if the SSE stream is dead (e.g. after a server
        // restart the proxy may not forward the connection close), the
        // agent's response would never arrive via deltas. Poll the
        // authoritative snapshot every few seconds until the session is
        // no longer active. If SSE is healthy, the polls are no-ops
        // (same revision) and stop once the turn completes.
        // Clear any previous safety-net poll so rapid prompts don't
        // stack independent intervals.
        if (activePromptPollIntervalRef.current !== null) {
          clearInterval(activePromptPollIntervalRef.current);
        }
        if (activePromptPollTimeoutRef.current !== null) {
          clearTimeout(activePromptPollTimeoutRef.current);
        }
        activePromptPollIntervalRef.current = setInterval(async () => {
          if (!isMountedRef.current) {
            if (activePromptPollIntervalRef.current !== null) {
              clearInterval(activePromptPollIntervalRef.current);
              activePromptPollIntervalRef.current = null;
            }
            return;
          }
          try {
            const freshState = await fetchState();
            if (!isMountedRef.current) {
              if (activePromptPollIntervalRef.current !== null) {
                clearInterval(activePromptPollIntervalRef.current);
                activePromptPollIntervalRef.current = null;
              }
              return;
            }
            // Allow revision downgrade so a restarted server (whose
            // persisted revision may be lower) is adopted.
            adoptState(freshState, {
              force: true,
              allowRevisionDowngrade: true,
            });
            // Stop once the session is no longer active.
            if (
              !freshState.sessions?.some(
                (session) =>
                  session.id === sessionId && session.status === "active",
              )
            ) {
              if (activePromptPollIntervalRef.current !== null) {
                clearInterval(activePromptPollIntervalRef.current);
                activePromptPollIntervalRef.current = null;
              }
            }
          } catch {
            // Best-effort; next interval will retry.
          }
        }, 3000);
        // Hard cap: stop polling after 5 minutes regardless.
        activePromptPollTimeoutRef.current = setTimeout(() => {
          if (activePromptPollIntervalRef.current !== null) {
            clearInterval(activePromptPollIntervalRef.current);
            activePromptPollIntervalRef.current = null;
          }
        }, 5 * 60 * 1000);
      } catch (error) {
        let restoredDraft = false;
        let restoredAttachments = false;

        setDraftsBySessionId((current) => {
          if (!draftText || (current[sessionId] ?? "") !== "") {
            return current;
          }

          restoredDraft = true;
          return {
            ...current,
            [sessionId]: draftText,
          };
        });
        setDraftAttachmentsBySessionId((current) => {
          if (
            attachments.length === 0 ||
            (current[sessionId]?.length ?? 0) > 0
          ) {
            return current;
          }

          restoredAttachments = true;
          return {
            ...current,
            [sessionId]: attachments,
          };
        });
        if (!restoredAttachments) {
          releaseDraftAttachments(attachments);
        }
        reportRequestError(error);
      } finally {
        setSendingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    })();

    return true;
  }

  function handleDraftAttachmentsAdd(
    sessionId: string,
    attachments: DraftImageAttachment[],
  ) {
    setDraftAttachmentsBySessionId((current) => ({
      ...current,
      [sessionId]: [...(current[sessionId] ?? []), ...attachments],
    }));
  }

  function handleDraftAttachmentRemove(
    sessionId: string,
    attachmentId: string,
  ) {
    setDraftAttachmentsBySessionId((current) => {
      const existing = current[sessionId];
      if (!existing) {
        return current;
      }

      const removed = existing.filter(
        (attachment) => attachment.id === attachmentId,
      );
      if (removed.length === 0) {
        return current;
      }

      releaseDraftAttachments(removed);
      const nextAttachments = existing.filter(
        (attachment) => attachment.id !== attachmentId,
      );
      if (nextAttachments.length === 0) {
        const nextState = { ...current };
        delete nextState[sessionId];
        return nextState;
      }

      return {
        ...current,
        [sessionId]: nextAttachments,
      };
    });
  }

  function openCreateSessionDialog(
    preferredPaneId: string | null = null,
    defaultProjectSelectionId: string | null = null,
  ) {
    const normalizedDefaultProjectSelectionId =
      defaultProjectSelectionId?.trim() ?? "";
    const fallbackProjectId =
      selectedProjectId !== ALL_PROJECTS_FILTER_ID &&
      projectLookup.has(selectedProjectId)
        ? selectedProjectId
        : activeSession?.projectId && projectLookup.has(activeSession.projectId)
          ? activeSession.projectId
          : CREATE_SESSION_WORKSPACE_ID;
    const defaultProjectId =
      normalizedDefaultProjectSelectionId === ALL_PROJECTS_FILTER_ID
        ? CREATE_SESSION_WORKSPACE_ID
        : normalizedDefaultProjectSelectionId &&
            projectLookup.has(normalizedDefaultProjectSelectionId)
          ? normalizedDefaultProjectSelectionId
          : fallbackProjectId;

    setCreateSessionPaneId(preferredPaneId ?? workspace.activePaneId);
    setCreateSessionProjectId(defaultProjectId);
    setRequestError(null);
    setIsCreateSessionOpen(true);
  }

  async function handleNewSession({
    agent,
    model,
    preferredPaneId = null,
    projectSelectionId = CREATE_SESSION_WORKSPACE_ID,
  }: {
    agent: AgentType;
    model: string;
    preferredPaneId?: string | null;
    projectSelectionId?: string;
  }) {
    const trimmedModel = model.trim();
    if (!trimmedModel && !usesSessionModelPicker(agent)) {
      setRequestError("Choose a model.");
      return false;
    }
    if (
      projectSelectionId === CREATE_SESSION_WORKSPACE_ID &&
      !!activeSession?.projectId &&
      !projectLookup.has(activeSession.projectId)
    ) {
      setRequestError(
        "The current workspace project is unavailable. Choose a project before creating a session.",
      );
      return false;
    }
    const workspaceProject =
      projectSelectionId === CREATE_SESSION_WORKSPACE_ID &&
      activeSession?.projectId &&
      projectLookup.has(activeSession.projectId)
        ? (projectLookup.get(activeSession.projectId) ?? null)
        : null;
    const targetProject =
      projectSelectionId !== CREATE_SESSION_WORKSPACE_ID
        ? (projectLookup.get(projectSelectionId) ?? null)
        : workspaceProject &&
            !isLocalRemoteId(resolveProjectRemoteId(workspaceProject))
          ? workspaceProject
          : null;
    const targetUsesRemoteProject =
      !!targetProject &&
      !isLocalRemoteId(resolveProjectRemoteId(targetProject));
    const readiness = targetUsesRemoteProject
      ? null
      : agentReadinessByAgent.get(agent);
    if (readiness?.blocking) {
      setRequestError(readiness.detail);
      return false;
    }

    setIsCreating(true);
    try {
      const targetPaneId = preferredPaneId ?? workspace.activePaneId;
      const targetProjectId =
        projectSelectionId === CREATE_SESSION_WORKSPACE_ID
          ? null
          : projectSelectionId;
      const created = await createSession({
        agent,
        model: usesSessionModelPicker(agent) ? undefined : trimmedModel,
        approvalPolicy:
          agent === "Codex" ? defaultCodexApprovalPolicy : undefined,
        reasoningEffort:
          agent === "Codex" ? defaultCodexReasoningEffort : undefined,
        cursorMode: agent === "Cursor" ? defaultCursorMode : undefined,
        claudeApprovalMode:
          agent === "Claude" ? defaultClaudeApprovalMode : undefined,
        claudeEffort: agent === "Claude" ? defaultClaudeEffort : undefined,
        geminiApprovalMode:
          agent === "Gemini" ? defaultGeminiApprovalMode : undefined,
        sandboxMode: agent === "Codex" ? defaultCodexSandboxMode : undefined,
        projectId: targetProjectId ?? targetProject?.id ?? undefined,
        workdir:
          targetProjectId || targetProject ? undefined : activeSession?.workdir,
      });
      if (!isMountedRef.current) {
        return false;
      }
      const adopted = adoptState(created.state, {
        openSessionId: created.sessionId,
        paneId: targetPaneId,
      });
      if (!adopted) {
        setWorkspace((current) =>
          applyControlPanelLayout(
            openSessionInWorkspaceState(
              current,
              created.sessionId,
              targetPaneId,
            ),
          ),
        );
      }
      if (
        agent === "Claude" ||
        agent === "Codex" ||
        agent === "Cursor" ||
        agent === "Gemini"
      ) {
        await handleRefreshSessionModelOptions(created.sessionId);
        if (!isMountedRef.current) {
          return false;
        }
      }
      setRequestError(null);
      return true;
    } catch (error) {
      if (!isMountedRef.current) {
        return false;
      }
      reportRequestError(error);
      return false;
    } finally {
      if (isMountedRef.current) {
        setIsCreating(false);
      }
    }
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

  function openCreateProjectDialog() {
    setNewProjectRemoteId(LOCAL_REMOTE_ID);
    setRequestError(null);
    setIsCreateProjectOpen(true);
  }

  async function handleCreateProject() {
    const rootPath = newProjectRootPath.trim();
    if (!rootPath) {
      setRequestError("Enter a project root path.");
      return false;
    }

    setIsCreatingProject(true);
    try {
      const created = await createProject({
        rootPath,
        remoteId: newProjectRemoteId,
      });
      adoptState(created.state);
      setSelectedProjectId(created.projectId);
      setNewProjectRootPath("");
      setNewProjectRemoteId(LOCAL_REMOTE_ID);
      setRequestError(null);
      return true;
    } catch (error) {
      reportRequestError(error);
      return false;
    } finally {
      setIsCreatingProject(false);
    }
  }

  async function handlePickProjectRoot() {
    if (!newProjectUsesLocalRemote) {
      setRequestError(
        "Remote projects need a path from the remote machine. Enter it manually.",
      );
      return;
    }

    setIsCreatingProject(true);
    try {
      const response = await pickProjectRoot();
      if (response.path) {
        setNewProjectRootPath(response.path);
        setRequestError(null);
      }
    } catch (error) {
      reportRequestError(error);
    } finally {
      setIsCreatingProject(false);
    }
  }

  async function handleApprovalDecision(
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) {
    try {
      const state = await submitApproval(sessionId, messageId, decision);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      reportRequestError(error);
    }
  }

  async function handleUserInputSubmit(
    sessionId: string,
    messageId: string,
    answers: Record<string, string[]>,
  ) {
    try {
      const state = await submitUserInput(sessionId, messageId, answers);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      reportRequestError(error);
    }
  }

  async function handleMcpElicitationSubmit(
    sessionId: string,
    messageId: string,
    action: McpElicitationAction,
    content?: JsonValue,
  ) {
    try {
      const state = await submitMcpElicitation(
        sessionId,
        messageId,
        action,
        content,
      );
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      reportRequestError(error);
    }
  }

  async function handleCodexAppRequestSubmit(
    sessionId: string,
    messageId: string,
    result: JsonValue,
  ) {
    try {
      const state = await submitCodexAppRequest(sessionId, messageId, result);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      reportRequestError(error);
    }
  }

  async function handleCancelQueuedPrompt(sessionId: string, promptId: string) {
    setSessions((current) => {
      const next = removeQueuedPromptFromSessions(current, sessionId, promptId);
      sessionsRef.current = next;
      return next;
    });
    try {
      const state = await cancelQueuedPrompt(sessionId, promptId);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      try {
        const state = await fetchState();
        adoptState(state);
      } catch {
        // Keep the original request error below; state refresh is best-effort.
      }
      reportRequestError(error);
    }
  }

  async function handleStopSession(sessionId: string) {
    setStoppingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const state = await stopSession(sessionId);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      reportRequestError(error);
    } finally {
      setStoppingSessionIds((current) =>
        setSessionFlag(current, sessionId, false),
      );
    }
  }

  function handleKillSession(
    sessionId: string,
    trigger?: HTMLButtonElement | null,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return;
    }

    closePendingSessionRename();
    clearPendingKillCloseTimeout();
    pendingKillTriggerRef.current = trigger ?? null;
    setPendingKillSessionId((current) =>
      current === sessionId ? null : sessionId,
    );
  }

  async function executeKillSession(sessionId: string) {
    setKillingSessionIds((current) => setSessionFlag(current, sessionId, true));
    try {
      const state = await killSession(sessionId);
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
    } finally {
      if (isMountedRef.current) {
        setKillingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function confirmKillSession() {
    if (!pendingKillSessionId) {
      return;
    }

    const sessionId = pendingKillSessionId;
    setPendingKillSessionId(null);
    setKillRevealSessionId(null);

    await executeKillSession(sessionId);
  }

  function focusPendingKillTrigger() {
    window.requestAnimationFrame(() => {
      pendingKillTriggerRef.current?.focus();
    });
  }

  function clearPendingKillCloseTimeout() {
    if (pendingKillCloseTimeoutRef.current === null) {
      return;
    }

    window.clearTimeout(pendingKillCloseTimeoutRef.current);
    pendingKillCloseTimeoutRef.current = null;
  }

  function schedulePendingKillConfirmationClose() {
    clearPendingKillCloseTimeout();

    const sessionId = pendingKillSessionId;
    if (!sessionId) {
      return;
    }

    pendingKillCloseTimeoutRef.current = window.setTimeout(() => {
      pendingKillCloseTimeoutRef.current = null;
      setPendingKillSessionId((current) =>
        current === sessionId ? null : current,
      );
      setPendingKillPopoverStyle(null);
    }, PENDING_KILL_CLOSE_DELAY_MS);
  }

  function closePendingKillConfirmation(restoreFocus = false) {
    clearPendingKillCloseTimeout();
    setPendingKillSessionId(null);
    setPendingKillPopoverStyle(null);
    if (restoreFocus) {
      focusPendingKillTrigger();
    }
  }

  function clearPendingSessionRenameCloseTimeout() {
    if (pendingSessionRenameCloseTimeoutRef.current === null) {
      return;
    }

    window.clearTimeout(pendingSessionRenameCloseTimeoutRef.current);
    pendingSessionRenameCloseTimeoutRef.current = null;
  }

  function schedulePendingSessionRenameClose() {
    clearPendingSessionRenameCloseTimeout();

    const pendingRename = pendingSessionRename;
    if (!pendingRename) {
      return;
    }
    if (pendingSessionRenameInputRef.current === document.activeElement) {
      return;
    }

    pendingSessionRenameCloseTimeoutRef.current = window.setTimeout(() => {
      pendingSessionRenameCloseTimeoutRef.current = null;
      setPendingSessionRename((current) =>
        current?.sessionId === pendingRename.sessionId ? null : current,
      );
      setPendingSessionRenameDraft("");
      setPendingSessionRenameStyle(null);
    }, PENDING_SESSION_RENAME_CLOSE_DELAY_MS);
  }

  function handleSessionRenameRequest(
    sessionId: string,
    clientX: number,
    clientY: number,
    trigger?: HTMLElement | null,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return;
    }

    closePendingKillConfirmation();
    clearPendingSessionRenameCloseTimeout();
    pendingSessionRenameTriggerRef.current = trigger ?? null;
    setPendingSessionRenameDraft(session.name);
    setPendingSessionRename({
      sessionId,
      clientX,
      clientY,
    });
  }

  function focusPendingSessionRenameTrigger() {
    window.requestAnimationFrame(() => {
      pendingSessionRenameTriggerRef.current?.focus();
    });
  }

  function closePendingSessionRename(restoreFocus = false) {
    clearPendingSessionRenameCloseTimeout();
    setPendingSessionRename(null);
    setPendingSessionRenameDraft("");
    setPendingSessionRenameStyle(null);
    if (restoreFocus) {
      focusPendingSessionRenameTrigger();
    }
  }

  async function confirmSessionRename() {
    if (!pendingSessionRename) {
      return;
    }

    const session = sessionLookup.get(pendingSessionRename.sessionId);
    const nextName = pendingSessionRenameDraft.trim();
    if (!session) {
      closePendingSessionRename();
      return;
    }
    if (!nextName) {
      return;
    }
    if (nextName === session.name.trim()) {
      closePendingSessionRename(true);
      return;
    }

    setUpdatingSessionIds((current) =>
      setSessionFlag(current, session.id, true),
    );
    try {
      const state = await renameSession(session.id, nextName);
      if (!isMountedRef.current) {
        return;
      }

      adoptState(state);
      setRequestError(null);
      closePendingSessionRename();
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }

      reportRequestError(error);
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, session.id, false),
        );
      }
    }
  }

  async function handlePendingSessionRenameNew() {
    if (!pendingSessionRename) {
      return;
    }

    const session = sessionLookup.get(pendingSessionRename.sessionId);
    if (!session) {
      closePendingSessionRename();
      return;
    }

    setIsCreating(true);
    try {
      const targetPaneId =
        findWorkspacePaneIdForSession(workspace, session.id) ??
        workspace.activePaneId;
      const created = await createSession({
        agent: session.agent,
        model: session.model,
        approvalPolicy:
          session.agent === "Codex"
            ? (session.approvalPolicy ?? defaultCodexApprovalPolicy)
            : undefined,
        reasoningEffort:
          session.agent === "Codex"
            ? (session.reasoningEffort ?? defaultCodexReasoningEffort)
            : undefined,
        cursorMode:
          session.agent === "Cursor"
            ? (session.cursorMode ?? defaultCursorMode)
            : undefined,
        claudeApprovalMode:
          session.agent === "Claude"
            ? (session.claudeApprovalMode ?? defaultClaudeApprovalMode)
            : undefined,
        claudeEffort:
          session.agent === "Claude"
            ? (session.claudeEffort ?? defaultClaudeEffort)
            : undefined,
        geminiApprovalMode:
          session.agent === "Gemini"
            ? (session.geminiApprovalMode ?? defaultGeminiApprovalMode)
            : undefined,
        sandboxMode:
          session.agent === "Codex"
            ? (session.sandboxMode ?? defaultCodexSandboxMode)
            : undefined,
        projectId:
          session.projectId && projectLookup.has(session.projectId)
            ? session.projectId
            : undefined,
        workdir: session.workdir,
      });
      if (!isMountedRef.current) {
        return;
      }

      const adopted = adoptState(created.state, {
        openSessionId: created.sessionId,
        paneId: targetPaneId,
      });
      if (!adopted) {
        setWorkspace((current) =>
          applyControlPanelLayout(
            openSessionInWorkspaceState(
              current,
              created.sessionId,
              targetPaneId,
            ),
          ),
        );
      }
      if (
        session.agent === "Claude" ||
        session.agent === "Codex" ||
        session.agent === "Cursor" ||
        session.agent === "Gemini"
      ) {
        await handleRefreshSessionModelOptions(created.sessionId);
      }
      setRequestError(null);
      closePendingSessionRename();
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }

      reportRequestError(error);
    } finally {
      if (isMountedRef.current) {
        setIsCreating(false);
      }
    }
  }

  async function handlePendingSessionRenameKill() {
    if (!pendingSessionRename) {
      return;
    }

    const session = sessionLookup.get(pendingSessionRename.sessionId);
    if (!session) {
      closePendingSessionRename();
      return;
    }

    closePendingSessionRename();
    setKillRevealSessionId(null);
    await executeKillSession(session.id);
  }

  async function handleSessionSettingsChange(
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return;
    }
    const normalizedModelValue =
      field === "model"
        ? normalizedRequestedSessionModel(session, value as string)
        : null;
    const payload =
      session.agent === "Codex"
        ? {
            ...(field === "model"
              ? { model: normalizedModelValue ?? (value as string) }
              : {}),
            reasoningEffort:
              field === "reasoningEffort"
                ? (value as CodexReasoningEffort)
                : normalizedCodexReasoningEffort(
                    session,
                    field === "model"
                      ? (normalizedModelValue ?? (value as string))
                      : session.model,
                  ),
            sandboxMode:
              field === "sandboxMode"
                ? (value as SandboxMode)
                : (session.sandboxMode ?? "workspace-write"),
            approvalPolicy:
              field === "approvalPolicy"
                ? (value as ApprovalPolicy)
                : (session.approvalPolicy ?? "never"),
          }
        : session.agent === "Cursor"
          ? field === "model"
            ? {
                model: normalizedModelValue ?? (value as string),
              }
            : field === "cursorMode"
              ? {
                  cursorMode: value as CursorMode,
                }
              : null
          : session.agent === "Claude"
            ? field === "model"
              ? {
                  model: normalizedModelValue ?? (value as string),
                }
              : field === "claudeApprovalMode"
                ? {
                    claudeApprovalMode: value as ClaudeApprovalMode,
                  }
                : field === "claudeEffort"
                  ? {
                      claudeEffort: value as ClaudeEffortLevel,
                    }
                  : null
            : session.agent === "Gemini"
              ? field === "model"
                ? {
                    model: normalizedModelValue ?? (value as string),
                  }
                : field === "geminiApprovalMode"
                  ? {
                      geminiApprovalMode: value as GeminiApprovalMode,
                    }
                  : null
              : null;
    if (!payload) {
      return;
    }

    const optimisticSession = buildOptimisticSessionSettingsUpdate(
      session,
      field,
      value,
    );
    const hasOptimisticUpdate = optimisticSession !== session;

    setRequestError(null);
    if (hasOptimisticUpdate) {
      updateSessionLocally(sessionId, () => optimisticSession);
    }
    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const state = await updateSessionSettings(sessionId, payload);
      adoptState(state);
      const updatedSession =
        state.sessions.find((entry) => entry.id === sessionId) ?? null;
      const nextNotice =
        session.agent === "Codex" && field === "model" && updatedSession
          ? describeCodexModelAdjustmentNotice(session, updatedSession)
          : null;
      setSessionSettingNotices((current) => {
        if (nextNotice) {
          return {
            ...current,
            [sessionId]: nextNotice,
          };
        }
        if (!current[sessionId]) {
          return current;
        }

        const nextState = { ...current };
        delete nextState[sessionId];
        return nextState;
      });
      setRequestError(null);
    } catch (error) {
      if (hasOptimisticUpdate) {
        updateSessionLocally(sessionId, (current) =>
          rollbackOptimisticSessionSettingsUpdate(
            current,
            session,
            optimisticSession,
          ),
        );
      }
      reportRequestError(error);
    } finally {
      setUpdatingSessionIds((current) =>
        setSessionFlag(current, sessionId, false),
      );
    }
  }

  async function handleRefreshSessionModelOptions(sessionId: string) {
    const previousSession =
      sessionsRef.current.find((entry) => entry.id === sessionId) ?? null;
    if (refreshingSessionModelOptionIdsRef.current[sessionId]) {
      return;
    }
    const nextRefreshingSessionIds = setSessionFlag(
      refreshingSessionModelOptionIdsRef.current,
      sessionId,
      true,
    );
    refreshingSessionModelOptionIdsRef.current = nextRefreshingSessionIds;
    setRefreshingSessionModelOptionIds(nextRefreshingSessionIds);

    setSessionModelOptionErrors((current) => {
      if (!current[sessionId]) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[sessionId];
      return nextState;
    });

    try {
      const state = await refreshSessionModelOptions(sessionId);
      if (!isMountedRef.current) {
        return;
      }
      adoptState(state);
      if (previousSession?.agent === "Codex") {
        const refreshedSession =
          state.sessions.find((entry) => entry.id === sessionId) ?? null;
        const nextNotice = refreshedSession
          ? describeCodexModelAdjustmentNotice(
              previousSession,
              refreshedSession,
            )
          : null;
        if (nextNotice) {
          setSessionSettingNotices((current) => ({
            ...current,
            [sessionId]: nextNotice,
          }));
        }
      }
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      const rawMessage = getErrorMessage(error);
      const session =
        sessionsRef.current.find((entry) => entry.id === sessionId) ?? null;
      const message = session
        ? describeSessionModelRefreshError(
            session.agent,
            rawMessage,
            agentReadinessByAgent.get(session.agent) ?? null,
          )
        : rawMessage;
      setSessionModelOptionErrors((current) => ({
        ...current,
        [sessionId]: message,
      }));
      reportRequestError(error, { message });
    } finally {
      if (!isMountedRef.current) {
        return;
      }
      const nextRefreshingSessionIds = setSessionFlag(
        refreshingSessionModelOptionIdsRef.current,
        sessionId,
        false,
      );
      refreshingSessionModelOptionIdsRef.current = nextRefreshingSessionIds;
      setRefreshingSessionModelOptionIds(nextRefreshingSessionIds);
    }
  }

  async function runCodexThreadStateAction(
    sessionId: string,
    request: () => Promise<StateResponse>,
    successNotice: string,
  ) {
    setRequestError(null);
    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const state = await request();
      if (!isMountedRef.current) {
        return;
      }
      adoptState(state);
      setSessionSettingNotices((current) => ({
        ...current,
        [sessionId]: successNotice,
      }));
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function handleForkCodexThread(
    sessionId: string,
    preferredPaneId: string | null,
  ) {
    setRequestError(null);
    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const created = await forkCodexThread(sessionId);
      if (!isMountedRef.current) {
        return;
      }
      const adopted = adoptState(created.state, {
        openSessionId: created.sessionId,
        paneId: preferredPaneId,
      });
      if (!adopted) {
        setWorkspace((current) =>
          applyControlPanelLayout(
            openSessionInWorkspaceState(
              current,
              created.sessionId,
              preferredPaneId,
            ),
          ),
        );
      }
      setSessionSettingNotices((current) => ({
        ...current,
        [sessionId]: "Forked the live Codex thread into a new session.",
        [created.sessionId]:
          "This session is attached to a forked Codex thread. Earlier Codex history was restored from Codex where available.",
      }));
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function handleArchiveCodexThread(sessionId: string) {
    await runCodexThreadStateAction(
      sessionId,
      () => archiveCodexThread(sessionId),
      "Archived the live Codex thread for this session.",
    );
  }

  async function handleUnarchiveCodexThread(sessionId: string) {
    await runCodexThreadStateAction(
      sessionId,
      () => unarchiveCodexThread(sessionId),
      "Restored the archived Codex thread for this session.",
    );
  }

  async function handleCompactCodexThread(sessionId: string) {
    await runCodexThreadStateAction(
      sessionId,
      () => compactCodexThread(sessionId),
      "Started Codex context compaction for this session.",
    );
  }

  async function handleRollbackCodexThread(
    sessionId: string,
    numTurns: number,
  ) {
    const turnLabel = numTurns === 1 ? "turn" : "turns";
    await runCodexThreadStateAction(
      sessionId,
      () => rollbackCodexThread(sessionId, numTurns),
      `Rolled the live Codex thread back by ${numTurns} ${turnLabel}.`,
    );
  }

  async function handleRefreshAgentCommands(sessionId: string) {
    if (refreshingAgentCommandSessionIdsRef.current[sessionId]) {
      return;
    }

    const nextRefreshingSessionIds = setSessionFlag(
      refreshingAgentCommandSessionIdsRef.current,
      sessionId,
      true,
    );
    refreshingAgentCommandSessionIdsRef.current = nextRefreshingSessionIds;
    setRefreshingAgentCommandSessionIds(nextRefreshingSessionIds);
    setAgentCommandErrors((current) => {
      if (!current[sessionId]) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[sessionId];
      return nextState;
    });

    try {
      const response = await fetchAgentCommands(sessionId);
      setAgentCommandsBySessionId((current) => ({
        ...current,
        [sessionId]: response.commands,
      }));
    } catch (error) {
      const message = getErrorMessage(error);
      setAgentCommandErrors((current) => ({
        ...current,
        [sessionId]: message,
      }));
    } finally {
      const nextRefreshingSessionIds = setSessionFlag(
        refreshingAgentCommandSessionIdsRef.current,
        sessionId,
        false,
      );
      refreshingAgentCommandSessionIdsRef.current = nextRefreshingSessionIds;
      setRefreshingAgentCommandSessionIds(nextRefreshingSessionIds);
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

  const pendingSessionRenameSession = pendingSessionRename
    ? (sessionLookup.get(pendingSessionRename.sessionId) ?? null)
    : null;
  const pendingSessionRenameValue = pendingSessionRenameDraft.trim();
  const isPendingSessionRenameSubmitting = pendingSessionRenameSession
    ? Boolean(updatingSessionIds[pendingSessionRenameSession.id])
    : false;
  const isPendingSessionRenameCreating = pendingSessionRenameSession
    ? isCreating
    : false;
  const isPendingSessionRenameKilling = pendingSessionRenameSession
    ? Boolean(killingSessionIds[pendingSessionRenameSession.id])
    : false;
  const pendingKillSession = pendingKillSessionId
    ? (sessionLookup.get(pendingKillSessionId) ?? null)
    : null;

  function handlePaneActivate(paneId: string) {
    setWorkspace((current) => activatePane(current, paneId));
  }

  function handlePaneTabSelect(paneId: string, tabId: string) {
    const pane = paneLookup.get(paneId);
    const tab = pane?.tabs.find((candidate) => candidate.id === tabId);
    if (tab?.kind === "session") {
      requestScrollToBottom(tab.sessionId);
    }

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

    setWorkspace((current) => activatePane(current, paneId, tabId));
  }

  function handleCloseTab(paneId: string, tabId: string) {
    setWorkspace((current) =>
      applyControlPanelLayout(closeWorkspaceTab(current, paneId, tabId)),
    );
  }

  function handleSplitPane(paneId: string, direction: "row" | "column") {
    setWorkspace((current) =>
      applyControlPanelLayout(splitPane(current, paneId, direction)),
    );
  }

  function handleSplitResizeStart(
    splitId: string,
    direction: "row" | "column",
    event: ReactPointerEvent<HTMLDivElement>,
  ) {
    event.preventDefault();
    event.stopPropagation();

    const container = event.currentTarget.parentElement;
    const ratio = getSplitRatio(workspace.root, splitId);
    if (!container || ratio === null) {
      return;
    }

    const rect = container.getBoundingClientRect();
    const { minRatio, maxRatio } = getWorkspaceSplitResizeBounds(
      workspace.root,
      splitId,
      direction,
      direction === "row" ? rect.width : rect.height,
      paneLookup,
    );
    resizeStateRef.current = {
      splitId,
      direction,
      startRatio: ratio,
      minRatio,
      maxRatio,
      startX: event.clientX,
      startY: event.clientY,
      size: direction === "row" ? rect.width : rect.height,
    };
  }

  function handleDraftChange(sessionId: string, nextValue: string) {
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

  function handleTabDragStart(drag: WorkspaceTabDrag) {
    draggedTabRef.current = drag;
    setDraggedTab(drag);
    broadcastTabDragMessage({
      type: "drag-start",
      payload: drag,
    });
  }

  function handleTabDragEnd() {
    const endedDrag = draggedTabRef.current;
    draggedTabRef.current = null;
    setDraggedTab(null);
    if (!endedDrag) {
      return;
    }

    broadcastTabDragMessage({
      type: "drag-end",
      dragId: endedDrag.dragId,
      sourceWindowId: endedDrag.sourceWindowId,
    });
  }

  function handleControlPanelLauncherDragStart(
    event: ReactDragEvent<HTMLButtonElement>,
    paneId: string,
    sectionId: ControlPanelSectionId,
    tab: WorkspaceTab,
  ) {
    const drag = createWorkspaceTabDrag(
      windowId,
      `control-panel-launcher:${paneId}:${sectionId}`,
      tab,
    );
    event.dataTransfer.effectAllowed = "copyMove";
    attachWorkspaceTabDragData(event.dataTransfer, drag);
    launcherDraggedTabRef.current = drag;
    // Defer the React state update - Chrome cancels in-progress drags when DOM
    // mutations happen during or immediately after dragstart.  setTimeout pushes
    // the re-render to the next task, after Chrome has committed the drag.
    setTimeout(() => setLauncherDraggedTab(drag), 0);
  }

  function handleControlPanelLauncherDragEnd() {
    launcherDraggedTabRef.current = null;
    setLauncherDraggedTab(null);
  }

  function clearStaleTabDragState() {
    const endedDrag = draggedTabRef.current;
    draggedTabRef.current = null;
    setDraggedTab(null);
    launcherDraggedTabRef.current = null;
    setLauncherDraggedTab(null);
    setExternalDraggedTab(null);
    if (!endedDrag) {
      return;
    }

    broadcastTabDragMessage({
      type: "drag-end",
      dragId: endedDrag.dragId,
      sourceWindowId: endedDrag.sourceWindowId,
    });
  }

  function recoverFromLostTabDrag(buttons: number) {
    if (buttons !== 0) {
      return false;
    }

    if (
      !draggedTabRef.current &&
      !launcherDraggedTabRef.current &&
      !externalDraggedTab
    ) {
      return false;
    }

    clearStaleTabDragState();
    return true;
  }

  function handleTabDrop(
    targetPaneId: string,
    placement: TabDropPlacement,
    tabIndex?: number,
    dataTransfer?: DataTransfer | null,
  ) {
    const droppedSession = readSessionDragData(dataTransfer ?? null);
    if (droppedSession) {
      startTransition(() => {
        setWorkspace((current) => {
          const nextWorkspace = placeSessionDropInWorkspaceState(
            current,
            droppedSession.sessionId,
            targetPaneId,
            placement,
            tabIndex,
          );
          return applyControlPanelLayout(nextWorkspace, controlPanelSide);
        });
      });
      return;
    }

    const parsedDrag = readWorkspaceTabDragData(dataTransfer);
    const sameWindowParsedDrag =
      parsedDrag && parsedDrag.sourceWindowId === windowId ? parsedDrag : null;
    const parsedLauncherDrag = sameWindowParsedDrag?.sourcePaneId.startsWith(
      "control-panel-launcher:",
    )
      ? sameWindowParsedDrag
      : null;
    const parsedPaneDrag =
      sameWindowParsedDrag &&
      !sameWindowParsedDrag.sourcePaneId.startsWith("control-panel-launcher:")
        ? sameWindowParsedDrag
        : null;
    const currentDraggedTab =
      draggedTabRef.current ?? draggedTab ?? parsedPaneDrag;
    const currentLauncherDraggedTab =
      launcherDraggedTabRef.current ?? launcherDraggedTab ?? parsedLauncherDrag;
    const currentExternalDraggedTab =
      externalDraggedTab ??
      (parsedDrag && parsedDrag.sourceWindowId !== windowId
        ? parsedDrag
        : null);

    if (currentDraggedTab) {
      const drop = currentDraggedTab;
      draggedTabRef.current = null;
      setDraggedTab(null);
      const nextControlPanelSide =
        drop.tab.kind === "controlPanel" &&
        (placement === "left" || placement === "right")
          ? placement
          : controlPanelSide;
      if (nextControlPanelSide !== controlPanelSide) {
        setControlPanelSide(nextControlPanelSide);
      }
      startTransition(() => {
        setWorkspace((current) =>
          applyControlPanelLayout(
            placeDraggedTab(
              current,
              drop.sourcePaneId,
              drop.tabId,
              targetPaneId,
              placement,
              tabIndex,
            ),
            nextControlPanelSide,
          ),
        );
      });
      return;
    }

    if (currentLauncherDraggedTab) {
      const drop = currentLauncherDraggedTab;
      launcherDraggedTabRef.current = null;
      setLauncherDraggedTab(null);
      flushSync(() => {
        setWorkspace((current) =>
          applyControlPanelLayout(
            placeExternalTab(
              current,
              drop.tab,
              targetPaneId,
              placement,
              tabIndex,
            ),
          ),
        );
      });
      return;
    }

    if (!currentExternalDraggedTab) {
      return;
    }

    const drop = currentExternalDraggedTab;
    setExternalDraggedTab((current) =>
      current?.dragId === drop.dragId ? null : current,
    );
    const nextControlPanelSide =
      drop.tab.kind === "controlPanel" &&
      (placement === "left" || placement === "right")
        ? placement
        : controlPanelSide;
    if (nextControlPanelSide !== controlPanelSide) {
      setControlPanelSide(nextControlPanelSide);
    }
    // Only ask the source window to remove its tab after this window has applied the drop.
    flushSync(() => {
      setWorkspace((current) =>
        applyControlPanelLayout(
          placeExternalTab(
            current,
            drop.tab,
            targetPaneId,
            placement,
            tabIndex,
          ),
          nextControlPanelSide,
        ),
      );
    });
    broadcastTabDragMessage({
      type: "drop-commit",
      dragId: drop.dragId,
      sourceWindowId: drop.sourceWindowId,
      sourcePaneId: drop.sourcePaneId,
      tabId: drop.tabId,
      targetWindowId: windowId,
    });
    broadcastTabDragMessage({
      type: "drag-end",
      dragId: drop.dragId,
      sourceWindowId: drop.sourceWindowId,
    });
  }

  useEffect(() => {
    if (!draggedTab && !launcherDraggedTab && !externalDraggedTab) {
      return;
    }

    const handleWindowBlur = () => {
      clearStaleTabDragState();
    };
    const handlePageHide = () => {
      clearStaleTabDragState();
    };
    const handleVisibilityChange = () => {
      if (document.visibilityState === "hidden") {
        clearStaleTabDragState();
      }
    };
    const timeoutId = window.setTimeout(() => {
      clearStaleTabDragState();
    }, TAB_DRAG_STALE_TIMEOUT_MS);

    window.addEventListener("blur", handleWindowBlur);
    window.addEventListener("pagehide", handlePageHide);
    document.addEventListener("visibilitychange", handleVisibilityChange);

    return () => {
      window.clearTimeout(timeoutId);
      window.removeEventListener("blur", handleWindowBlur);
      window.removeEventListener("pagehide", handlePageHide);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
    };
  }, [draggedTab, launcherDraggedTab, externalDraggedTab]);

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

  function requestScrollToBottom(sessionId: string) {
    setPendingScrollToBottomRequest({
      sessionId,
      token: Date.now() + Math.random(),
    });
  }

  function handlePaneSourcePathChange(paneId: string, path: string) {
    setWorkspace((current) => setPaneSourcePath(current, paneId, path));
  }

  function handleOpenSourceTab(
    paneId: string,
    path: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: {
      line?: number;
      column?: number;
      openInNewTab?: boolean;
    },
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
    const gitSectionId = options?.sectionId ?? request.sectionId;
    const pendingTab = {
      changeType: pendingGitDiffPreviewChangeType(request.statusCode),
      changeSetId: null,
      diff: "",
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
      const errorMessage = getErrorMessage(error);
      setWorkspace((current) =>
        applyControlPanelLayout(
          updateGitDiffPreviewTabInWorkspaceState(
            current,
            requestKey,
            (tab) => ({
              ...tab,
              diff: "",
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

  function renderWorkspaceControlSurface(
    paneId: string,
    fixedSection: ControlPanelSectionId | null = null,
  ): JSX.Element {
    const surfaceId = fixedSection ? `${paneId}-${fixedSection}` : paneId;
    const controlPanelProjectFilterId = `control-panel-project-scope-${surfaceId}`;
    const controlSurfaceCollapsedOrchestratorIds =
      collapsedSessionOrchestratorIdsBySurfaceId[surfaceId] ?? [];
    const controlSurfacePane = paneLookup.get(paneId) ?? null;
    const controlSurfaceActiveTab = controlSurfacePane
      ? (controlSurfacePane.tabs.find(
          (tab) => tab.id === controlSurfacePane.activeTabId,
        ) ??
        controlSurfacePane.tabs[0] ??
        null)
      : null;
    const controlSurfaceOriginSession =
      controlSurfaceActiveTab &&
      "originSessionId" in controlSurfaceActiveTab &&
      controlSurfaceActiveTab.originSessionId
        ? (sessionLookup.get(controlSurfaceActiveTab.originSessionId) ?? null)
        : null;
    const controlSurfacePaneSession = controlSurfacePane?.activeSessionId
      ? (sessionLookup.get(controlSurfacePane.activeSessionId) ?? null)
      : null;
    const nearestSessionPaneId = findNearestSessionPaneId(workspace, paneId);
    const nearestSessionPane = nearestSessionPaneId
      ? (paneLookup.get(nearestSessionPaneId) ?? null)
      : null;
    const nearestSessionTab = nearestSessionPane
      ? (nearestSessionPane.tabs.find(
          (tab) => tab.id === nearestSessionPane.activeTabId,
        ) ??
        nearestSessionPane.tabs[0] ??
        null)
      : null;
    const nearestSession =
      nearestSessionTab?.kind === "session"
        ? (sessionLookup.get(nearestSessionTab.sessionId) ?? null)
        : null;
    const isStandaloneControlSurface = fixedSection !== null;
    const standaloneControlSurfaceTabId =
      isStandaloneControlSurface && controlSurfaceActiveTab
        ? controlSurfaceActiveTab.id
        : null;
    const standaloneControlSurfaceViewState = standaloneControlSurfaceTabId
      ? (standaloneControlSurfaceViewStateByTabId[
          standaloneControlSurfaceTabId
        ] ?? null)
      : null;
    const controlSurfaceTabProjectId = resolveWorkspaceTabProjectId(
      controlSurfaceActiveTab ?? undefined,
      sessionLookup,
    );
    const controlSurfaceSelectedProjectId = isStandaloneControlSurface
      ? (standaloneControlSurfaceViewState?.projectId ??
        (controlSurfaceTabProjectId &&
        projectLookup.has(controlSurfaceTabProjectId)
          ? controlSurfaceTabProjectId
          : ALL_PROJECTS_FILTER_ID))
      : selectedProjectId;
    const controlSurfaceSelectedProject =
      controlSurfaceSelectedProjectId === ALL_PROJECTS_FILTER_ID
        ? null
        : (projectLookup.get(controlSurfaceSelectedProjectId) ?? null);
    const controlSurfaceSessionCandidates = (
      isStandaloneControlSurface
        ? [controlSurfaceOriginSession, controlSurfacePaneSession]
        : [
            controlSurfaceOriginSession,
            controlSurfacePaneSession,
            nearestSession,
            activeSession,
          ]
    ).filter((session): session is Session => Boolean(session));
    const controlSurfaceSession = controlSurfaceSelectedProject
      ? (controlSurfaceSessionCandidates.find(
          (session) => session.projectId === controlSurfaceSelectedProject.id,
        ) ??
        sessions.find(
          (session) => session.projectId === controlSurfaceSelectedProject.id,
        ) ??
        null)
      : (controlSurfaceSessionCandidates[0] ?? sessions[0] ?? null);
    const controlPanelLauncherOriginProjectId =
      controlSurfaceSelectedProject?.id ??
      controlSurfaceSession?.projectId ??
      controlSurfaceTabProjectId ??
      null;
    const controlPanelLauncherOriginSessionId =
      controlSurfaceSession?.id ?? null;
    const controlSurfaceWorkspaceRoot = resolveControlPanelWorkspaceRoot(
      controlSurfaceSelectedProject,
      controlSurfaceSession?.workdir ?? null,
    );
    const isStandaloneSessionList =
      fixedSection === "sessions" && standaloneControlSurfaceTabId !== null;
    const standaloneSessionListState = isStandaloneSessionList
      ? buildControlSurfaceSessionListState(
          sessions,
          controlSurfaceSelectedProject,
          standaloneControlSurfaceViewState?.sessionListFilter ?? "all",
          standaloneControlSurfaceViewState?.sessionListSearchQuery ?? "",
        )
      : null;
    const controlSurfaceSessionListFilter = standaloneSessionListState
      ? (standaloneControlSurfaceViewState?.sessionListFilter ?? "all")
      : sessionListFilter;
    const controlSurfaceSessionListSearchQuery = standaloneSessionListState
      ? (standaloneControlSurfaceViewState?.sessionListSearchQuery ?? "")
      : sessionListSearchQuery;
    const controlSurfaceSessionFilterCounts = standaloneSessionListState
      ? standaloneSessionListState.sessionFilterCounts
      : sessionFilterCounts;
    const controlSurfaceHasSessionListSearch = standaloneSessionListState
      ? standaloneSessionListState.hasSessionListSearch
      : hasSessionListSearch;
    const controlSurfaceSessionListSearchResults = standaloneSessionListState
      ? standaloneSessionListState.sessionListSearchResults
      : sessionListSearchResults;
    const controlSurfaceFilteredSessions = standaloneSessionListState
      ? standaloneSessionListState.filteredSessions
      : filteredSessions;
    const controlSurfaceSessionListEntries =
      buildControlSurfaceSessionListEntries(
        controlSurfaceFilteredSessions,
        orchestrators,
      );
    const controlSurfaceFilesystemRoot =
      controlSurfaceActiveTab?.kind === "filesystem"
        ? controlSurfaceActiveTab.rootPath
        : fixedSection === "files"
          ? controlSurfaceWorkspaceRoot
          : controlPanelFilesystemRoot;
    const controlSurfaceGitWorkdir =
      controlSurfaceActiveTab?.kind === "gitStatus"
        ? controlSurfaceActiveTab.workdir
        : fixedSection === "git"
          ? controlSurfaceWorkspaceRoot
          : controlPanelGitWorkdir;

    function renderControlSurfaceSessionRow(session: Session) {
      const isActive = session.id === activeSession?.id;
      const isOpen = openSessionIds.has(session.id);
      const isKilling = Boolean(killingSessionIds[session.id]);
      const isKillConfirmationOpen = pendingKillSessionId === session.id;
      const isKillVisible =
        isKilling ||
        isKillConfirmationOpen ||
        killRevealSessionId === session.id;
      const searchResult = controlSurfaceSessionListSearchResults.get(session.id);

      return (
        <div
          key={`${surfaceId}-${session.id}`}
          className={`session-row-shell ${isActive ? "selected" : ""} ${isOpen ? "open" : ""} ${isKillVisible ? "kill-armed" : ""}`}
          onMouseLeave={() => {
            if (!isKilling && !isKillConfirmationOpen) {
              setKillRevealSessionId((current) =>
                current === session.id ? null : current,
              );
            }
          }}
          onBlur={(event) => {
            const nextTarget = event.relatedTarget;
            if (
              !isKilling &&
              !isKillConfirmationOpen &&
              (!(nextTarget instanceof Node) ||
                !event.currentTarget.contains(nextTarget))
            ) {
              setKillRevealSessionId((current) =>
                current === session.id ? null : current,
              );
            }
          }}
        >
          <button
            className={`session-row ${isActive ? "selected" : ""} ${isOpen ? "open" : ""}`}
            type="button"
            draggable
            onClick={() =>
              handleSidebarSessionClick(
                session.id,
                paneId,
                !fixedSection,
              )
            }
            title={`${session.agent} / ${session.workdir}`}
            onDragStart={(event) => {
              event.dataTransfer.effectAllowed = "copy";
              attachSessionDragData(
                event.dataTransfer,
                session.id,
                session.name,
              );
              const rect = event.currentTarget.getBoundingClientRect();
              event.dataTransfer.setDragImage(
                event.currentTarget,
                Math.max(12, event.clientX - rect.left),
                Math.max(12, event.clientY - rect.top),
              );
            }}
            onContextMenu={(event) => {
              event.preventDefault();
              handleSessionRenameRequest(
                session.id,
                event.clientX,
                event.clientY,
                event.currentTarget,
              );
            }}
          >
            <div className="session-copy">
              <div className="session-title-line">
                <strong>{session.name}</strong>
                {searchResult ? (
                  <span className="session-search-count">
                    {searchResult.matchCount} hit
                    {searchResult.matchCount === 1 ? "" : "s"}
                  </span>
                ) : null}
              </div>
              <div
                className={`session-preview${searchResult ? " session-preview-search-result" : ""}`}
                title={searchResult?.snippet ?? session.preview}
              >
                {searchResult?.snippet ?? session.preview}
              </div>
            </div>
          </button>
          <button
            className="session-row-status-button"
            type="button"
            onClick={() =>
              setKillRevealSessionId((current) =>
                current === session.id && !isKilling
                  ? null
                  : session.id,
              )
            }
            aria-label={`Show session actions for ${session.name}`}
          >
            <span
              className="status-agent-badge session-row-status-badge"
              data-status={session.status}
            >
              <AgentIcon
                agent={session.agent}
                className="session-row-status-icon"
              />
            </span>
          </button>
          <button
            className="ghost-button session-row-kill"
            type="button"
            onClick={(event) => {
              handleKillSession(
                session.id,
                event.currentTarget,
              );
            }}
            disabled={isKilling}
            aria-expanded={isKillConfirmationOpen}
            aria-controls={
              isKillConfirmationOpen
                ? `kill-session-popover-${session.id}`
                : undefined
            }
            aria-label={`Kill ${session.name}`}
          >
            {isKilling ? "Killing" : "Kill"}
          </button>
        </div>
      );
    }

    function buildControlPanelLauncherTab(sectionId: ControlPanelSectionId) {
      return createControlPanelSectionLauncherTab(sectionId, {
        filesystemRoot: controlSurfaceFilesystemRoot,
        gitWorkdir: controlSurfaceGitWorkdir,
        originProjectId: controlPanelLauncherOriginProjectId,
        originSessionId: controlPanelLauncherOriginSessionId,
      });
    }
    function handleControlPanelSectionTabDragStart(
      event: ReactDragEvent<HTMLButtonElement>,
      sectionId: ControlPanelSectionId,
    ) {
      const tab = buildControlPanelLauncherTab(sectionId);
      if (!tab) {
        return;
      }

      handleControlPanelLauncherDragStart(event, paneId, sectionId, tab);
    }

    function updateStandaloneControlSurfaceViewState(
      updates: Partial<StandaloneControlSurfaceViewState>,
    ) {
      if (!standaloneControlSurfaceTabId) {
        return;
      }

      setStandaloneControlSurfaceViewStateByTabId((current) => {
        const previous = current[standaloneControlSurfaceTabId] ?? {};
        const next = { ...previous, ...updates };
        if (
          previous.projectId === next.projectId &&
          previous.sessionListFilter === next.sessionListFilter &&
          previous.sessionListSearchQuery === next.sessionListSearchQuery
        ) {
          return current;
        }

        return {
          ...current,
          [standaloneControlSurfaceTabId]: next,
        };
      });
    }

    function toggleControlSurfaceOrchestratorGroup(orchestratorId: string) {
      setCollapsedSessionOrchestratorIdsBySurfaceId((current) => {
        const previous = current[surfaceId] ?? [];
        const next = previous.includes(orchestratorId)
          ? previous.filter((candidateId) => candidateId !== orchestratorId)
          : [...previous, orchestratorId];

        if (!next.length) {
          if (!(surfaceId in current)) {
            return current;
          }

          const { [surfaceId]: _discard, ...rest } = current;
          return rest;
        }

        return {
          ...current,
          [surfaceId]: next,
        };
      });
    }

    function handleControlSurfaceProjectScopeChange(nextProjectId: string) {
      if (!isStandaloneControlSurface || !controlSurfaceActiveTab) {
        setSelectedProjectId(nextProjectId);
        return;
      }

      updateStandaloneControlSurfaceViewState({ projectId: nextProjectId });
      const nextSelectedProject =
        nextProjectId !== ALL_PROJECTS_FILTER_ID &&
        projectLookup.has(nextProjectId)
          ? (projectLookup.get(nextProjectId) ?? null)
          : null;
      const preferredStandaloneSession =
        controlSurfaceOriginSession ?? controlSurfacePaneSession ?? null;
      const nextScopedSession = nextSelectedProject
        ? preferredStandaloneSession?.projectId === nextSelectedProject.id
          ? preferredStandaloneSession
          : (sessions.find(
              (session) => session.projectId === nextSelectedProject.id,
            ) ?? null)
        : (preferredStandaloneSession ?? sessions[0] ?? null);
      const nextWorkspaceRoot = resolveControlPanelWorkspaceRoot(
        nextSelectedProject,
        nextScopedSession?.workdir ?? null,
      );

      setWorkspace((current) =>
        rescopeControlSurfacePane(
          current,
          paneId,
          nextScopedSession?.id ?? null,
          nextSelectedProject?.id ?? null,
          nextWorkspaceRoot,
        ),
      );
    }

    function renderControlPanelProjectScope() {
      return (
        <div className="control-panel-scope-control">
          <label
            className="control-panel-scope-label"
            htmlFor={controlPanelProjectFilterId}
          >
            Project
          </label>
          <ThemedCombobox
            id={controlPanelProjectFilterId}
            className="control-panel-scope-combobox"
            value={controlSurfaceSelectedProjectId}
            options={controlPanelProjectOptions}
            onChange={handleControlSurfaceProjectScopeChange}
            aria-label="Project"
          />
        </div>
      );
    }

    function renderOpenTabAction(
      sectionId: ControlPanelSectionId,
      onClick: () => void,
      disabled: boolean,
      tab: WorkspaceTab | null,
    ): JSX.Element {
      return (
        <button
          className="control-panel-header-action control-panel-header-open-button"
          type="button"
          draggable={!disabled && tab !== null}
          aria-label="Open tab"
          title={
            disabled ? "Open tab" : "Open tab or drag it into the workspace"
          }
          onClick={onClick}
          onDragStart={(event) => {
            if (!tab) {
              event.preventDefault();
              return;
            }

            handleControlPanelLauncherDragStart(event, paneId, sectionId, tab);
          }}
          onDragEnd={handleControlPanelLauncherDragEnd}
          disabled={disabled}
        >
          <span
            className="control-panel-header-action-icon control-panel-header-action-icon-open-tab"
            aria-hidden="true"
          >
            <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
              <path
                d="M3.5 4.25h4l1.15 1.25h4A1.25 1.25 0 0 1 13.9 6.75v5.5a1.25 1.25 0 0 1-1.25 1.25H3.5A1.25 1.25 0 0 1 2.25 12.25v-6.75A1.25 1.25 0 0 1 3.5 4.25Z"
                fill="none"
                stroke="currentColor"
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth="1.35"
              />
              <path
                d="M8.75 3.25v4.5M6.5 5.5h4.5"
                fill="none"
                stroke="currentColor"
                strokeLinecap="round"
                strokeWidth="1.35"
              />
            </svg>
          </span>
        </button>
      );
    }

    function renderCanvasTabAction(onClick: () => void): JSX.Element {
      return (
        <button
          className="control-panel-header-action control-panel-header-open-button"
          type="button"
          onClick={onClick}
          aria-label="Canvas"
          title="Canvas"
        >
          <span
            className="control-panel-header-action-icon control-panel-header-action-icon-open-tab"
            aria-hidden="true"
          >
            <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
              <rect
                x="2.5"
                y="4"
                width="11"
                height="7.5"
                rx="0.8"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.35"
              />
              <path
                d="M5.5 11.5L4 14.5M10.5 11.5l1.5 3M8 11.5V14"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.2"
                strokeLinecap="round"
              />
              <path
                d="M7 2v2M9 2v2"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.2"
                strokeLinecap="round"
              />
            </svg>
          </span>
        </button>
      );
    }

    function renderControlPanelHeaderActions(sectionId: ControlPanelSectionId) {
      switch (sectionId) {
        case "files":
          return fixedSection
            ? null
            : renderOpenTabAction(
                "files",
                () =>
                  handleOpenFilesystemTab(
                    paneId,
                    controlSurfaceFilesystemRoot,
                    controlPanelLauncherOriginSessionId,
                    controlPanelLauncherOriginProjectId,
                  ),
                !(controlSurfaceFilesystemRoot?.trim() ?? ""),
                buildControlPanelLauncherTab("files"),
              );

        case "git":
          return (
            <>
              {fixedSection
                ? null
                : renderOpenTabAction(
                    "git",
                    () =>
                      handleOpenGitStatusTab(
                        paneId,
                        controlSurfaceGitWorkdir,
                        controlPanelLauncherOriginSessionId,
                        controlPanelLauncherOriginProjectId,
                      ),
                    !(controlSurfaceGitWorkdir?.trim() ?? ""),
                    buildControlPanelLauncherTab("git"),
                  )}
            </>
          );

        case "projects":
          return (
            <>
              {fixedSection
                ? null
                : renderOpenTabAction(
                    "projects",
                    () =>
                      handleOpenProjectListTab(
                        paneId,
                        controlPanelLauncherOriginSessionId,
                        controlPanelLauncherOriginProjectId,
                      ),
                    false,
                    buildControlPanelLauncherTab("projects"),
                  )}
              <button
                className="control-panel-header-action control-panel-header-new-session-button"
                type="button"
                onClick={() => openCreateProjectDialog()}
                aria-label="Add project"
                title="Add project"
                disabled={isCreatingProject}
              >
                <span
                  className="control-panel-header-action-icon control-panel-header-action-icon-new"
                  aria-hidden="true"
                >
                  <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
                    <circle
                      cx="8"
                      cy="8"
                      r="6.5"
                      fill="none"
                      stroke="currentColor"
                      strokeWidth="1.3"
                    />
                    <path
                      d="M8 5v6M5 8h6"
                      stroke="currentColor"
                      strokeWidth="1.3"
                      strokeLinecap="round"
                    />
                  </svg>
                </span>
              </button>
            </>
          );

        case "orchestrators":
          return (
            <>
              {fixedSection
                ? null
                : renderOpenTabAction(
                    "orchestrators",
                    () =>
                      handleOpenOrchestratorListTab(
                        paneId,
                        controlPanelLauncherOriginSessionId,
                        controlPanelLauncherOriginProjectId,
                      ),
                    false,
                    buildControlPanelLauncherTab("orchestrators"),
                  )}
              <button
                className="control-panel-header-action control-panel-header-new-session-button"
                type="button"
                onClick={() =>
                  handleOpenOrchestratorCanvasTab(
                    paneId,
                    controlPanelLauncherOriginSessionId,
                    controlPanelLauncherOriginProjectId,
                    { startMode: "new" },
                  )
                }
              >
                <span
                  className="control-panel-header-action-icon control-panel-header-action-icon-canvas"
                  aria-hidden="true"
                >
                  <svg
                    viewBox="-9 0 64 64"
                    focusable="false"
                    aria-hidden="true"
                  >
                    <g
                      transform="translate(1,1)"
                      stroke="currentColor"
                      strokeWidth="3.5"
                      fill="none"
                    >
                      <path d="M12.5,45 L7.8,62 L2.9,62 L7.6,45" />
                      <path d="M30.5,45 L35.2,62 L40.1,62 L35.4,45" />
                      <rect x="20" y="45" width="4" height="11" />
                      <rect x="19" y="0" width="4" height="9" />
                      <path d="M42,37 C43.1,37 44,37.9 44,39 L44,43 C44,44.1 43.1,45 42,45 L2,45 C0.9,45 0,44.1 0,43 L0,39 C0,37.9 0.9,37 2,37" />
                      <path d="M40.2,41 L4,41 C2.9,41 2,40.1 2,39 L2,11 C2,9.9 2.9,9 4,9 L40.2,9 C41.3,9 42,9.9 42,11 L42,39 C42,40.1 41.3,41 40.2,41 Z" />
                    </g>
                    <line
                      x1="24"
                      y1="20"
                      x2="34"
                      y2="20"
                      stroke="currentColor"
                      strokeWidth="3"
                      strokeLinecap="round"
                    />
                    <line
                      x1="29"
                      y1="15"
                      x2="29"
                      y2="25"
                      stroke="currentColor"
                      strokeWidth="3"
                      strokeLinecap="round"
                    />
                  </svg>
                </span>
              </button>
            </>
          );

        case "sessions":
          return (
            <>
              {fixedSection
                ? null
                : renderOpenTabAction(
                    "sessions",
                    () =>
                      handleOpenSessionListTab(
                        paneId,
                        controlPanelLauncherOriginSessionId,
                        controlPanelLauncherOriginProjectId,
                      ),
                    false,
                    buildControlPanelLauncherTab("sessions"),
                  )}
              {renderCanvasTabAction(() =>
                handleOpenCanvasTab(
                  paneId,
                  controlPanelLauncherOriginSessionId,
                  controlPanelLauncherOriginProjectId,
                ),
              )}
              <button
                className="control-panel-header-action control-panel-header-new-session-button"
                type="button"
                onClick={() =>
                  openCreateSessionDialog(
                    paneId,
                    controlSurfaceSelectedProjectId,
                  )
                }
                aria-label="New"
                title="New session"
                aria-haspopup="dialog"
                aria-expanded={isCreateSessionOpen}
                aria-controls="create-session-dialog"
                disabled={isCreating}
              >
                <span
                  className="control-panel-header-action-icon control-panel-header-action-icon-new"
                  aria-hidden="true"
                >
                  <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
                    <circle
                      cx="8"
                      cy="8"
                      r="6.5"
                      fill="none"
                      stroke="currentColor"
                      strokeWidth="1.3"
                    />
                    <path
                      d="M8 5v6M5 8h6"
                      stroke="currentColor"
                      strokeWidth="1.3"
                      strokeLinecap="round"
                    />
                  </svg>
                </span>
              </button>
            </>
          );

        default:
          return null;
      }
    }

    function renderControlPanelSection(sectionId: ControlPanelSectionId) {
      switch (sectionId) {
        case "files":
          return (
            <section
              className="control-panel-section-stack control-panel-section-files"
              aria-label="Files"
            >
              {renderControlPanelProjectScope()}
              <FileSystemPanel
                rootPath={controlSurfaceFilesystemRoot}
                sessionId={controlPanelLauncherOriginSessionId}
                projectId={controlPanelLauncherOriginProjectId}
                workspaceFilesChangedEvent={workspaceFilesChangedEvent}
                showPathControls={false}
                onOpenPath={(path, options) =>
                  handleOpenSourceTab(
                    paneId,
                    path,
                    controlPanelLauncherOriginSessionId,
                    controlPanelLauncherOriginProjectId,
                    options,
                  )
                }
                onOpenRootPath={(path) => {
                  if (!fixedSection) {
                    setControlPanelFilesystemRoot(path.trim() || null);
                  }
                }}
              />
            </section>
          );

        case "orchestrators":
          return (
            <OrchestratorTemplateLibraryPanel
              orchestrators={orchestrators}
              onStateUpdated={handleOrchestratorStateUpdated}
              onNewCanvas={() =>
                handleOpenOrchestratorCanvasTab(
                  paneId,
                  controlPanelLauncherOriginSessionId,
                  controlPanelLauncherOriginProjectId,
                  { startMode: "new" },
                )
              }
              onOpenCanvas={(templateId) =>
                handleOpenOrchestratorCanvasTab(
                  paneId,
                  controlPanelLauncherOriginSessionId,
                  controlPanelLauncherOriginProjectId,
                  { templateId },
                )
              }
            />
          );

        case "git":
          return (
            <section
              className="control-panel-section-stack control-panel-section-git"
              aria-label="Git status"
            >
              {renderControlPanelProjectScope()}
              <GitStatusPanel
                projectId={controlPanelLauncherOriginProjectId}
                sessionId={controlPanelLauncherOriginSessionId}
                workdir={controlSurfaceGitWorkdir}
                showPathControls={false}
                onStatusChange={(status) =>
                  setControlPanelGitStatusCount(status?.files.length ?? 0)
                }
                onOpenDiff={(diff, options) =>
                  handleOpenGitStatusDiffPreviewTab(
                    paneId,
                    diff,
                    controlPanelLauncherOriginSessionId,
                    controlPanelLauncherOriginProjectId,
                    options,
                  )
                }
                onOpenWorkdir={(path) => {
                  if (!fixedSection) {
                    setControlPanelGitWorkdir(path.trim() || null);
                  }
                }}
              />
            </section>
          );

        case "projects":
          return (
            <section
              className="control-panel-section-stack"
              aria-label="Projects"
            >
              <section className="project-controls" aria-label="Projects">
                <div className="project-controls-header">
                  <div className="session-control-label">Projects</div>
                  <span className="project-count-badge">{projects.length}</span>
                </div>
                <div className="project-list" role="list">
                  <button
                    className={`project-row ${controlSurfaceSelectedProjectId === ALL_PROJECTS_FILTER_ID ? "selected" : ""}`}
                    type="button"
                    onClick={() =>
                      handleControlSurfaceProjectScopeChange(
                        ALL_PROJECTS_FILTER_ID,
                      )
                    }
                  >
                    <span className="project-row-copy">
                      <strong>All projects</strong>
                      <span className="project-row-path">
                        Show every session in this window.
                      </span>
                    </span>
                    <span className="project-row-count">{sessions.length}</span>
                  </button>
                  {projects.map((project) => {
                    const isSelected =
                      project.id === controlSurfaceSelectedProjectId;

                    return (
                      <button
                        key={project.id}
                        className={`project-row ${isSelected ? "selected" : ""}`}
                        type="button"
                        onClick={() =>
                          handleControlSurfaceProjectScopeChange(project.id)
                        }
                      >
                        <span className="project-row-copy">
                          <strong>{project.name}</strong>
                          <span className="project-row-path">
                            {describeProjectScope(project, remoteLookup)}
                          </span>
                        </span>
                        <span className="project-row-count">
                          {projectSessionCounts.get(project.id) ?? 0}
                        </span>
                      </button>
                    );
                  })}
                </div>
              </section>
            </section>
          );

        case "sessions":
        default:
          return (
            <section
              className="control-panel-section-stack control-panel-section-sessions"
              aria-label="Sessions"
            >
              <section className="session-list-shell" aria-label="Sessions">
                <div className="session-list-tools">
                  {renderControlPanelProjectScope()}
                  <input
                    ref={fixedSection ? undefined : sessionListSearchInputRef}
                    className="themed-input session-list-search-input"
                    type="search"
                    value={controlSurfaceSessionListSearchQuery}
                    placeholder="Search sessions"
                    spellCheck={false}
                    aria-label="Search sessions"
                    title={`Search across visible sessions (${primaryModifierLabel()}+Shift+F)`}
                    onChange={(event) => {
                      if (standaloneControlSurfaceTabId) {
                        updateStandaloneControlSurfaceViewState({
                          sessionListSearchQuery: event.currentTarget.value,
                        });
                      } else {
                        setSessionListSearchQuery(event.currentTarget.value);
                      }
                    }}
                    onKeyDown={(event) => {
                      if (event.key === "Escape") {
                        event.preventDefault();
                        if (controlSurfaceSessionListSearchQuery) {
                          if (standaloneControlSurfaceTabId) {
                            updateStandaloneControlSurfaceViewState({
                              sessionListSearchQuery: "",
                            });
                          } else {
                            setSessionListSearchQuery("");
                          }
                        } else {
                          event.currentTarget.blur();
                        }
                      }
                    }}
                  />
                  {controlSurfaceHasSessionListSearch ? (
                    <div
                      className="session-list-search-meta"
                      aria-live="polite"
                    >
                      {controlSurfaceFilteredSessions.length === 1
                        ? "1 matching session"
                        : `${controlSurfaceFilteredSessions.length} matching sessions`}
                    </div>
                  ) : null}
                </div>
                <div className="session-list">
                  {controlSurfaceFilteredSessions.length > 0 ? (
                    controlSurfaceSessionListEntries.map((entry) => {
                      if (entry.kind === "session") {
                        return renderControlSurfaceSessionRow(entry.session);
                      }

                      const groupName = formatSessionOrchestratorGroupName(
                        entry.orchestrator,
                      );

                      const isGroupCollapsed =
                        controlSurfaceCollapsedOrchestratorIds.includes(
                          entry.orchestrator.id,
                        );
                      const groupListId =
                        `${surfaceId}-orchestrator-group-list-${entry.orchestrator.id}`;
                      const pendingOrchestratorAction =
                        pendingOrchestratorActionById[entry.orchestrator.id];
                      const hasPendingOrchestratorAction =
                        Boolean(pendingOrchestratorAction);

                      return (
                        <section
                          key={`${surfaceId}-orchestrator-group-${entry.orchestrator.id}`}
                          className="session-orchestrator-group"
                          role="group"
                          aria-label={`Orchestration ${groupName}`}
                          data-status={entry.orchestrator.status}
                        >
                          <header className="session-orchestrator-group-header">
                            <button
                              className="session-orchestrator-group-toggle"
                              type="button"
                              onClick={() =>
                                toggleControlSurfaceOrchestratorGroup(
                                  entry.orchestrator.id,
                                )
                              }
                              aria-expanded={!isGroupCollapsed}
                              aria-controls={
                                !isGroupCollapsed ? groupListId : undefined
                              }
                              aria-label={`${isGroupCollapsed ? "Expand" : "Collapse"} ${groupName} sessions`}
                              title={isGroupCollapsed ? "Expand sessions" : "Collapse sessions"}
                            >
                              <svg
                                className={`session-orchestrator-group-chevron${!isGroupCollapsed ? " expanded" : ""}`}
                                viewBox="0 0 12 12"
                                focusable="false"
                                aria-hidden="true"
                              >
                                <path
                                  d="M4 2.75 7.75 6 4 9.25"
                                  fill="none"
                                  stroke="currentColor"
                                  strokeLinecap="round"
                                  strokeLinejoin="round"
                                  strokeWidth="1.4"
                                />
                              </svg>
                            </button>
                            <div className="session-orchestrator-group-copy">
                              <span className="session-orchestrator-group-label">
                                Orchestration
                              </span>
                              <div className="session-orchestrator-group-title-row">
                                <strong className="session-orchestrator-group-name">
                                  {groupName}
                                </strong>
                                <span className="session-orchestrator-group-count">
                                  {entry.sessions.length === 1
                                    ? "1 session"
                                    : `${entry.sessions.length} sessions`}
                                </span>
                              </div>
                            </div>
                            <div className="session-orchestrator-group-meta">
                              {entry.orchestrator.status === "running" ? (
                                <div className="session-orchestrator-group-actions">
                                  <OrchestratorRuntimeActionButton
                                    action="pause"
                                    orchestratorId={entry.orchestrator.id}
                                    isPending={
                                      pendingOrchestratorAction === "pause"
                                    }
                                    disabled={hasPendingOrchestratorAction}
                                    onClick={() =>
                                      void handleOrchestratorRuntimeAction(
                                        entry.orchestrator.id,
                                        "pause",
                                      )
                                    }
                                  />
                                  <OrchestratorRuntimeActionButton
                                    action="stop"
                                    orchestratorId={entry.orchestrator.id}
                                    isPending={
                                      pendingOrchestratorAction === "stop"
                                    }
                                    disabled={hasPendingOrchestratorAction}
                                    onClick={() =>
                                      void handleOrchestratorRuntimeAction(
                                        entry.orchestrator.id,
                                        "stop",
                                      )
                                    }
                                  />
                                </div>
                              ) : entry.orchestrator.status === "paused" ? (
                                <div className="session-orchestrator-group-actions">
                                  <OrchestratorRuntimeActionButton
                                    action="resume"
                                    orchestratorId={entry.orchestrator.id}
                                    isPending={
                                      pendingOrchestratorAction === "resume"
                                    }
                                    disabled={hasPendingOrchestratorAction}
                                    onClick={() =>
                                      void handleOrchestratorRuntimeAction(
                                        entry.orchestrator.id,
                                        "resume",
                                      )
                                    }
                                  />
                                  <OrchestratorRuntimeActionButton
                                    action="stop"
                                    orchestratorId={entry.orchestrator.id}
                                    isPending={
                                      pendingOrchestratorAction === "stop"
                                    }
                                    disabled={hasPendingOrchestratorAction}
                                    onClick={() =>
                                      void handleOrchestratorRuntimeAction(
                                        entry.orchestrator.id,
                                        "stop",
                                      )
                                    }
                                  />
                                </div>
                              ) : null}
                            </div>
                          </header>
                          {!isGroupCollapsed ? (
                            <div
                              id={groupListId}
                              className="session-orchestrator-group-list"
                            >
                              {entry.sessions.map((session) =>
                                renderControlSurfaceSessionRow(session),
                              )}
                            </div>
                          ) : null}
                        </section>
                      );
                    })
                  ) : (
                    <div className="session-filter-empty">
                      {sessions.length === 0
                        ? "No sessions yet."
                        : controlSurfaceHasSessionListSearch
                          ? controlSurfaceSelectedProject
                            ? `No sessions match this search in ${controlSurfaceSelectedProject.name}.`
                            : "No sessions match this search."
                          : controlSurfaceSelectedProject
                            ? `No ${controlSurfaceSessionListFilter === "all" ? "" : `${controlSurfaceSessionListFilter} `}sessions in ${controlSurfaceSelectedProject.name}.`
                            : "No sessions match this filter."}
                    </div>
                  )}
                </div>
              </section>

              <section className="sidebar-status" aria-label="Session filters">
                <div className="session-control-label">Status</div>
                <div className="sidebar-status-chips">
                  <button
                    className={`chip sidebar-status-chip ${controlSurfaceSessionListFilter === "all" ? "selected" : ""}`}
                    type="button"
                    onClick={() => {
                      if (standaloneControlSurfaceTabId) {
                        updateStandaloneControlSurfaceViewState({
                          sessionListFilter: "all",
                        });
                      } else {
                        setSessionListFilter("all");
                      }
                    }}
                    aria-pressed={controlSurfaceSessionListFilter === "all"}
                  >
                    No filter ({controlSurfaceSessionFilterCounts.all})
                  </button>
                  <button
                    className={`chip sidebar-status-chip ${controlSurfaceSessionListFilter === "working" ? "selected" : ""}`}
                    type="button"
                    onClick={() => {
                      if (standaloneControlSurfaceTabId) {
                        updateStandaloneControlSurfaceViewState({
                          sessionListFilter: "working",
                        });
                      } else {
                        setSessionListFilter("working");
                      }
                    }}
                    aria-pressed={controlSurfaceSessionListFilter === "working"}
                  >
                    Working ({controlSurfaceSessionFilterCounts.working})
                  </button>
                  <button
                    className={`chip sidebar-status-chip ${controlSurfaceSessionListFilter === "asking" ? "selected" : ""}`}
                    type="button"
                    onClick={() => {
                      if (standaloneControlSurfaceTabId) {
                        updateStandaloneControlSurfaceViewState({
                          sessionListFilter: "asking",
                        });
                      } else {
                        setSessionListFilter("asking");
                      }
                    }}
                    aria-pressed={controlSurfaceSessionListFilter === "asking"}
                  >
                    Asking ({controlSurfaceSessionFilterCounts.asking})
                  </button>
                  <button
                    className={`chip sidebar-status-chip ${controlSurfaceSessionListFilter === "completed" ? "selected" : ""}`}
                    type="button"
                    onClick={() => {
                      if (standaloneControlSurfaceTabId) {
                        updateStandaloneControlSurfaceViewState({
                          sessionListFilter: "completed",
                        });
                      } else {
                        setSessionListFilter("completed");
                      }
                    }}
                    aria-pressed={
                      controlSurfaceSessionListFilter === "completed"
                    }
                  >
                    Completed ({controlSurfaceSessionFilterCounts.completed})
                  </button>
                </div>
              </section>
            </section>
          );
      }
    }

    return (
      <div className="sidebar sidebar-panel">
        <ControlPanelSurface
          ref={fixedSection ? undefined : controlPanelSurfaceRef}
          fixedSection={fixedSection}
          gitStatusCount={controlPanelGitStatusCount}
          isPreferencesOpen={isSettingsOpen}
          onOpenPreferences={() => setIsSettingsOpen(true)}
          onSectionTabDragEnd={handleControlPanelLauncherDragEnd}
          onSectionTabDragStart={handleControlPanelSectionTabDragStart}
          projectCount={projects.length}
          sessionCount={projectScopedSessions.length}
          renderHeaderActions={renderControlPanelHeaderActions}
          renderSection={renderControlPanelSection}
          sectionLauncherTabs={{
            files: buildControlPanelLauncherTab("files"),
            git: buildControlPanelLauncherTab("git"),
            projects: buildControlPanelLauncherTab("projects"),
            sessions: buildControlPanelLauncherTab("sessions"),
            orchestrators: buildControlPanelLauncherTab("orchestrators"),
          }}
          windowId={windowId}
          launcherPaneId={paneId}
        />
      </div>
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
              draftsBySessionId={draftsBySessionId}
              draftAttachmentsBySessionId={draftAttachmentsBySessionId}
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
              renderControlPanelPaneBarStatus={
                renderControlPanelPaneBarStatus
              }
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
      {pendingKillSession && typeof document !== "undefined"
        ? createPortal(
            <>
              <div
                className="session-kill-popover-backdrop"
                onPointerMove={() => {
                  schedulePendingKillConfirmationClose();
                }}
                onPointerDown={(event) => {
                  event.preventDefault();
                  closePendingKillConfirmation();
                }}
              />
              <div
                ref={pendingKillPopoverRef}
                id={`kill-session-popover-${pendingKillSession.id}`}
                className="session-kill-popover panel"
                style={
                  pendingKillPopoverStyle ?? {
                    left: 0,
                    top: 0,
                    visibility: "hidden",
                  }
                }
                role="dialog"
                aria-label={`Confirm killing ${pendingKillSession.name}`}
                onPointerEnter={() => {
                  clearPendingKillCloseTimeout();
                }}
                onPointerLeave={() => {
                  schedulePendingKillConfirmationClose();
                }}
              >
                <div className="session-kill-popover-actions">
                  <button
                    className="ghost-button session-kill-popover-cancel"
                    type="button"
                    onClick={() => {
                      closePendingKillConfirmation(true);
                    }}
                  >
                    Cancel
                  </button>
                  <button
                    ref={pendingKillConfirmButtonRef}
                    className="send-button session-kill-popover-confirm"
                    type="button"
                    onClick={() => void confirmKillSession()}
                  >
                    Kill
                  </button>
                </div>
              </div>
            </>,
            document.body,
          )
        : null}
      {pendingSessionRenameSession && typeof document !== "undefined"
        ? createPortal(
            <>
              <div
                className="session-rename-popover-backdrop"
                onPointerMove={() => {
                  schedulePendingSessionRenameClose();
                }}
                onPointerDown={(event) => {
                  event.preventDefault();
                  closePendingSessionRename();
                }}
              />
              <form
                ref={pendingSessionRenamePopoverRef}
                className="session-rename-popover panel"
                style={
                  pendingSessionRenameStyle ?? {
                    left: 0,
                    top: 0,
                    visibility: "hidden",
                  }
                }
                onSubmit={(event) => {
                  event.preventDefault();
                  void confirmSessionRename();
                }}
                onPointerEnter={() => {
                  clearPendingSessionRenameCloseTimeout();
                }}
                onPointerLeave={() => {
                  schedulePendingSessionRenameClose();
                }}
              >
                <input
                  ref={pendingSessionRenameInputRef}
                  className="themed-input session-rename-input"
                  type="text"
                  value={pendingSessionRenameDraft}
                  maxLength={120}
                  spellCheck={false}
                  aria-label="Session name"
                  placeholder="Session name"
                  onFocus={() => {
                    clearPendingSessionRenameCloseTimeout();
                  }}
                  onChange={(event) => {
                    clearPendingSessionRenameCloseTimeout();
                    setPendingSessionRenameDraft(event.currentTarget.value);
                  }}
                />
                <div className="session-rename-actions">
                  <button
                    className="ghost-button session-rename-new"
                    type="button"
                    onClick={() => {
                      void handlePendingSessionRenameNew();
                    }}
                    disabled={
                      isPendingSessionRenameCreating ||
                      isPendingSessionRenameSubmitting ||
                      isPendingSessionRenameKilling
                    }
                  >
                    {isPendingSessionRenameCreating ? "Creating" : "New"}
                  </button>
                  <button
                    className="ghost-button session-rename-kill"
                    type="button"
                    onClick={() => {
                      void handlePendingSessionRenameKill();
                    }}
                    disabled={
                      isPendingSessionRenameCreating ||
                      isPendingSessionRenameSubmitting ||
                      isPendingSessionRenameKilling
                    }
                  >
                    {isPendingSessionRenameKilling ? "Killing" : "Kill"}
                  </button>
                  <button
                    className="ghost-button session-rename-cancel"
                    type="button"
                    onClick={() => {
                      closePendingSessionRename(true);
                    }}
                    disabled={
                      isPendingSessionRenameCreating ||
                      isPendingSessionRenameSubmitting ||
                      isPendingSessionRenameKilling
                    }
                  >
                    Cancel
                  </button>
                  <button
                    className="send-button session-rename-save"
                    type="submit"
                    disabled={
                      !pendingSessionRenameValue ||
                      isPendingSessionRenameCreating ||
                      isPendingSessionRenameSubmitting ||
                      isPendingSessionRenameKilling
                    }
                  >
                    {isPendingSessionRenameSubmitting ? "Saving" : "Save"}
                  </button>
                </div>
              </form>
            </>,
            document.body,
          )
        : null}
      {isCreateSessionOpen ? (
        <div
          className="dialog-backdrop"
          onMouseDown={() => {
            if (!isCreating) {
              setRequestError(null);
              setIsCreateSessionOpen(false);
            }
          }}
        >
          <section
            id="create-session-dialog"
            className="dialog-card panel create-session-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="create-session-dialog-title"
            onMouseDown={(event) => {
              event.stopPropagation();
            }}
          >
            <div className="create-session-dialog-header">
              <div>
                <div className="card-label">Session</div>
                <h2 id="create-session-dialog-title">New session</h2>
                <p className="dialog-copy">
                  Pick the assistant, project, and any startup settings before
                  opening the session. Session-specific controls stay with the
                  session after it starts.
                </p>
              </div>

              <button
                className="ghost-button settings-dialog-close"
                type="button"
                aria-label="Close dialog"
                title="Close"
                onClick={() => {
                  setRequestError(null);
                  setIsCreateSessionOpen(false);
                }}
                disabled={isCreating}
              >
                <DialogCloseIcon />
              </button>
            </div>

            <form
              className="create-session-dialog-body"
              onSubmit={(event) => {
                event.preventDefault();
                void handleCreateSessionDialogSubmit();
              }}
            >
              {requestError ? (
                <article className="thread-notice create-session-dialog-error">
                  <div className="card-label">Backend</div>
                  <p>{requestError}</p>
                </article>
              ) : null}

              <div className="create-session-field">
                <label
                  className="session-control-label"
                  htmlFor="create-session-agent"
                >
                  Assistant
                </label>
                <ThemedCombobox
                  id="create-session-agent"
                  value={newSessionAgent}
                  options={
                    NEW_SESSION_AGENT_OPTIONS as readonly ComboboxOption[]
                  }
                  onChange={(nextValue) =>
                    setNewSessionAgent(nextValue as AgentType)
                  }
                  disabled={isCreating}
                />
              </div>

              {createSessionUsesSessionModelPicker ? (
                <div className="create-session-field">
                  <label className="session-control-label">Model</label>
                  <p className="create-session-field-hint">
                    {createSessionModelHint(newSessionAgent)}
                  </p>
                </div>
              ) : (
                <div className="create-session-field">
                  <label
                    className="session-control-label"
                    htmlFor="create-session-model"
                  >
                    Model
                  </label>
                  <ThemedCombobox
                    id="create-session-model"
                    value={newSessionModel}
                    options={newSessionModelOptions}
                    onChange={(nextValue) =>
                      setNewSessionModelByAgent((current) => ({
                        ...current,
                        [newSessionAgent]: nextValue,
                      }))
                    }
                    disabled={isCreating}
                  />
                </div>
              )}

              {newSessionAgent === "Codex" ? (
                <div className="create-session-field">
                  <label
                    className="session-control-label"
                    htmlFor="create-session-codex-reasoning-effort"
                  >
                    Codex reasoning effort
                  </label>
                  <ThemedCombobox
                    id="create-session-codex-reasoning-effort"
                    value={defaultCodexReasoningEffort}
                    options={
                      CODEX_REASONING_EFFORT_OPTIONS as readonly ComboboxOption[]
                    }
                    onChange={(nextValue) =>
                      handleDefaultCodexReasoningEffortChange(
                        nextValue as CodexReasoningEffort,
                      )
                    }
                    disabled={isCreating}
                  />
                  <p className="create-session-field-hint">
                    New Codex sessions start with this reasoning effort, and you
                    can still change it per session later.
                  </p>
                </div>
              ) : null}

              {newSessionAgent === "Claude" ? (
                <div className="create-session-field">
                  <label
                    className="session-control-label"
                    htmlFor="create-session-claude-effort"
                  >
                    Claude effort
                  </label>
                  <ThemedCombobox
                    id="create-session-claude-effort"
                    value={defaultClaudeEffort}
                    options={CLAUDE_EFFORT_OPTIONS as readonly ComboboxOption[]}
                    onChange={(nextValue) =>
                      handleDefaultClaudeEffortChange(
                        nextValue as ClaudeEffortLevel,
                      )
                    }
                    disabled={isCreating}
                  />
                  <p className="create-session-field-hint">
                    New Claude sessions start with this effort, and you can
                    still change it per session later.
                  </p>
                </div>
              ) : null}

              {newSessionAgent === "Cursor" ? (
                <div className="create-session-field">
                  <label
                    className="session-control-label"
                    htmlFor="create-session-cursor-mode"
                  >
                    Cursor mode
                  </label>
                  <ThemedCombobox
                    id="create-session-cursor-mode"
                    value={defaultCursorMode}
                    options={CURSOR_MODE_OPTIONS as readonly ComboboxOption[]}
                    onChange={(nextValue) =>
                      setDefaultCursorMode(nextValue as CursorMode)
                    }
                    disabled={isCreating}
                  />
                  <p className="create-session-field-hint">
                    Agent auto-approves tool requests and can edit, Ask keeps
                    approval cards, and Plan stays read-only.
                  </p>
                </div>
              ) : null}

              {newSessionAgent === "Gemini" ? (
                <div className="create-session-field">
                  <label
                    className="session-control-label"
                    htmlFor="create-session-gemini-mode"
                  >
                    Gemini approvals
                  </label>
                  <ThemedCombobox
                    id="create-session-gemini-mode"
                    value={defaultGeminiApprovalMode}
                    options={
                      GEMINI_APPROVAL_OPTIONS as readonly ComboboxOption[]
                    }
                    onChange={(nextValue) =>
                      setDefaultGeminiApprovalMode(
                        nextValue as GeminiApprovalMode,
                      )
                    }
                    disabled={isCreating}
                  />
                  <p className="create-session-field-hint">
                    Default prompts for approval, Auto edit approves edit tools,
                    YOLO approves all tools, and Plan keeps Gemini read-only.
                  </p>
                </div>
              ) : null}

              <div className="create-session-field">
                <label
                  className="session-control-label"
                  htmlFor="create-session-project"
                >
                  Project
                </label>
                <ThemedCombobox
                  id="create-session-project"
                  value={createSessionProjectId}
                  options={createSessionProjectOptions}
                  onChange={setCreateSessionProjectId}
                  disabled={isCreating}
                />
                <p className="create-session-field-hint">
                  {createSessionProjectHint}
                </p>
              </div>

              {createSessionProjectSelectionError ? (
                <article className="thread-notice create-session-readiness">
                  <div className="card-label">Remote</div>
                  <p>{createSessionProjectSelectionError}</p>
                </article>
              ) : null}

              {createSessionAgentReadiness ? (
                <article className="thread-notice create-session-readiness">
                  <div className="card-label">
                    {createSessionAgentReadiness.blocking
                      ? "Setup Required"
                      : "Ready"}
                  </div>
                  <p>{createSessionAgentReadiness.detail}</p>
                  {createSessionAgentReadiness.commandPath ? (
                    <p className="create-session-field-hint">
                      Binary: {createSessionAgentReadiness.commandPath}
                    </p>
                  ) : null}
                </article>
              ) : null}

              {createSessionAgentReadiness?.warningDetail ? (
                <article className="thread-notice create-session-readiness">
                  <div className="card-label">Warning</div>
                  <p>{createSessionAgentReadiness.warningDetail}</p>
                </article>
              ) : null}

              <div className="dialog-actions create-session-dialog-actions">
                <button
                  className="ghost-button"
                  type="button"
                  onClick={() => {
                    setRequestError(null);
                    setIsCreateSessionOpen(false);
                  }}
                  disabled={isCreating}
                >
                  Cancel
                </button>
                <button
                  className="send-button create-session-submit"
                  type="submit"
                  disabled={
                    isCreating ||
                    createSessionBlocked ||
                    !!createSessionProjectSelectionError
                  }
                >
                  {isCreating ? "Creating..." : "Create session"}
                </button>
              </div>
            </form>
          </section>
        </div>
      ) : null}
      {isCreateProjectOpen ? (
        <div
          className="dialog-backdrop"
          onMouseDown={() => {
            if (!isCreatingProject) {
              setRequestError(null);
              setIsCreateProjectOpen(false);
            }
          }}
        >
          <section
            id="create-project-dialog"
            className="dialog-card panel create-project-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="create-project-dialog-title"
            onMouseDown={(event) => {
              event.stopPropagation();
            }}
          >
            <div className="create-project-dialog-header">
              <div>
                <div className="card-label">Project</div>
                <h2 id="create-project-dialog-title">Add project</h2>
                <p className="dialog-copy">
                  Choose a local folder or enter a remote root path to add a
                  scoped project.
                </p>
              </div>

              <button
                className="ghost-button settings-dialog-close"
                type="button"
                aria-label="Close dialog"
                title="Close"
                onClick={() => {
                  setRequestError(null);
                  setIsCreateProjectOpen(false);
                }}
                disabled={isCreatingProject}
              >
                <DialogCloseIcon />
              </button>
            </div>

            <form
              className="create-project-dialog-body"
              onSubmit={(event) => {
                event.preventDefault();
                void (async () => {
                  const created = await handleCreateProject();
                  if (created) {
                    setIsCreateProjectOpen(false);
                  }
                })();
              }}
            >
              {requestError ? (
                <article className="thread-notice create-project-dialog-error">
                  <div className="card-label">Backend</div>
                  <p>{requestError}</p>
                </article>
              ) : null}

              <div className="create-project-field">
                <label
                  className="session-control-label"
                  htmlFor="create-project-remote"
                >
                  Remote
                </label>
                <ThemedCombobox
                  id="create-project-remote"
                  value={newProjectRemoteId}
                  options={createProjectRemoteOptions}
                  onChange={setNewProjectRemoteId}
                  disabled={isCreatingProject}
                />
                <p className="create-session-field-hint">
                  {remoteDisplayName(
                    newProjectSelectedRemote,
                    newProjectRemoteId,
                  )}{" "}
                  - {remoteConnectionLabel(newProjectSelectedRemote)}
                </p>
              </div>

              <div className="create-project-field">
                <label
                  className="session-control-label"
                  htmlFor="create-project-root"
                >
                  {newProjectUsesLocalRemote ? "Folder" : "Remote root path"}
                </label>
                <input
                  id="create-project-root"
                  className="themed-input project-root-input"
                  type="text"
                  value={newProjectRootPath}
                  placeholder={
                    newProjectUsesLocalRemote
                      ? "/path/to/project"
                      : "/remote/path/to/project"
                  }
                  onChange={(event) =>
                    setNewProjectRootPath(event.target.value)
                  }
                  disabled={isCreatingProject}
                />
                <p className="create-session-field-hint">
                  {newProjectUsesLocalRemote
                    ? "Local projects use the folder picker and local filesystem panels immediately."
                    : "Remote projects store the remote path and route files and sessions through the local SSH proxy."}
                </p>
              </div>

              <div className="dialog-actions create-project-dialog-actions">
                <button
                  className="ghost-button"
                  type="button"
                  onClick={() => void handlePickProjectRoot()}
                  disabled={isCreatingProject || !newProjectUsesLocalRemote}
                >
                  Choose folder
                </button>
                <button
                  className="ghost-button"
                  type="button"
                  onClick={() => {
                    setRequestError(null);
                    setIsCreateProjectOpen(false);
                  }}
                  disabled={isCreatingProject}
                >
                  Cancel
                </button>
                <button
                  className="send-button create-project-submit"
                  type="submit"
                  disabled={isCreatingProject}
                >
                  {isCreatingProject ? "Adding..." : "Add project"}
                </button>
              </div>
            </form>
          </section>
        </div>
      ) : null}
      {isSettingsOpen ? (
        <div
          className="dialog-backdrop"
          onMouseDown={() => {
            setIsSettingsOpen(false);
          }}
        >
          <section
            id="settings-dialog"
            className="dialog-card panel settings-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="settings-dialog-title"
            onMouseDown={(event) => {
              event.stopPropagation();
            }}
          >
            <div className="settings-dialog-header">
              <div>
                <div className="card-label">Preferences</div>
                <h2 id="settings-dialog-title">Settings</h2>
                <p className="dialog-copy settings-dialog-copy">
                  Tune the interface and manage reusable orchestrator templates
                  without disturbing active sessions.
                </p>
              </div>

              <button
                className="ghost-button settings-dialog-close"
                type="button"
                aria-label="Close dialog"
                title="Close"
                onClick={() => {
                  setIsSettingsOpen(false);
                }}
              >
                <DialogCloseIcon />
              </button>
            </div>

            <div className="settings-dialog-body">
              <div
                className="settings-tab-list"
                role="tablist"
                aria-label="Preferences sections"
              >
                {PREFERENCES_TABS.map((tab) => {
                  const isSelected = settingsTab === tab.id;

                  return (
                    <button
                      key={tab.id}
                      id={`settings-tab-${tab.id}`}
                      className={`settings-tab ${isSelected ? "selected" : ""}`}
                      type="button"
                      role="tab"
                      aria-selected={isSelected}
                      aria-controls={`settings-panel-${tab.id}`}
                      onClick={() => setSettingsTab(tab.id)}
                    >
                      {tab.label}
                    </button>
                  );
                })}
              </div>

              <div
                id={`settings-panel-${settingsTab}`}
                className={`settings-tab-panel ${settingsTab === "themes" ? "theme-settings-panel" : ""}`.trim()}
                role="tabpanel"
                aria-labelledby={`settings-tab-${settingsTab}`}
              >
                {settingsTab === "themes" ? (
                  <ThemePreferencesPanel
                    activeStyle={activeStyle}
                    activeTheme={activeTheme}
                    styleId={styleId}
                    themeId={themeId}
                    onSelectStyle={setStyleId}
                    onSelectTheme={setThemeId}
                  />
                ) : settingsTab === "appearance" ? (
                  <AppearancePreferencesPanel
                    densityPercent={densityPercent}
                    editorFontSizePx={editorFontSizePx}
                    fontSizePx={fontSizePx}
                    onSelectDensity={(nextValue) =>
                      setDensityPercent(clampDensityPreference(nextValue))
                    }
                    onSelectEditorFontSize={(nextValue) =>
                      setEditorFontSizePx(
                        clampEditorFontSizePreference(nextValue),
                      )
                    }
                    onSelectFontSize={(nextValue) =>
                      setFontSizePx(clampFontSizePreference(nextValue))
                    }
                  />
                ) : settingsTab === "remotes" ? (
                  <RemotePreferencesPanel
                    remotes={remoteConfigs}
                    onSaveRemotes={(nextRemotes) => {
                      void persistAppPreferences({ remotes: nextRemotes });
                    }}
                  />
                ) : settingsTab === "orchestrators" ? (
                  <OrchestratorTemplatesPanel
                    projects={projects}
                    sessions={sessions}
                    onStateUpdated={handleOrchestratorStateUpdated}
                  />
                ) : settingsTab === "codex-prompts" ? (
                  <CodexPromptPreferencesPanel
                    defaultApprovalPolicy={defaultCodexApprovalPolicy}
                    defaultReasoningEffort={defaultCodexReasoningEffort}
                    defaultSandboxMode={defaultCodexSandboxMode}
                    onSelectApprovalPolicy={setDefaultCodexApprovalPolicy}
                    onSelectReasoningEffort={
                      handleDefaultCodexReasoningEffortChange
                    }
                    onSelectSandboxMode={setDefaultCodexSandboxMode}
                  />
                ) : (
                  <ClaudeApprovalsPreferencesPanel
                    defaultClaudeApprovalMode={defaultClaudeApprovalMode}
                    defaultClaudeEffort={defaultClaudeEffort}
                    onSelectEffort={handleDefaultClaudeEffortChange}
                    onSelectMode={setDefaultClaudeApprovalMode}
                  />
                )}
              </div>
            </div>
          </section>
        </div>
      ) : null}
    </div>
  );
}

function WorkspaceNodeView({
  node,
  codexState,
  projectLookup,
  remoteLookup,
  paneLookup,
  sessionLookup,
  activePaneId,
  isLoading,
  draftsBySessionId,
  draftAttachmentsBySessionId,
  sendingSessionIds,
  stoppingSessionIds,
  killingSessionIds,
  updatingSessionIds,
  refreshingSessionModelOptionIds,
  sessionModelOptionErrors,
  agentCommandsBySessionId,
  refreshingAgentCommandSessionIds,
  agentCommandErrors,
  sessionSettingNotices,
  paneShouldStickToBottomRef,
  paneScrollPositionsRef,
  paneContentSignaturesRef,
  pendingScrollToBottomRequest,
  windowId,
  draggedTab,
  getKnownDraggedTab,
  editorAppearance,
  editorFontSizePx,
  onActivatePane,
  onSelectTab,
  onCloseTab,
  onSplitPane,
  onResizeStart,
  onTabDragStart,
  onTabDragEnd,
  onTabDrop,
  onPaneViewModeChange,
  onOpenSourceTab,
  onOpenDiffPreviewTab,
  onOpenGitStatusDiffPreviewTab,
  onOpenFilesystemTab,
  onOpenGitStatusTab,
  onOpenInstructionDebuggerTab,
  onOpenCanvasTab,
  onUpsertCanvasSessionCard,
  onRemoveCanvasSessionCard,
  onSetCanvasZoom,
  onPaneSourcePathChange,
  onOpenConversationFromDiff,
  onInsertReviewIntoPrompt,
  onDraftCommit,
  onDraftAttachmentsAdd,
  onDraftAttachmentRemove,
  onComposerError,
  onSend,
  onCancelQueuedPrompt,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onStopSession,
  onKillSession,
  onRenameSessionRequest,
  onScrollToBottomRequestHandled,
  onSessionSettingsChange,
  onArchiveCodexThread,
  onCompactCodexThread,
  onForkCodexThread,
  onRefreshSessionModelOptions,
  onRefreshAgentCommands,
  onRollbackCodexThread,
  onUnarchiveCodexThread,
  onOrchestratorStateUpdated,
  renderControlPanel,
  renderControlPanelPaneBarStatus,
  renderControlPanelPaneBarActions,
  backendConnectionState,
  workspaceFilesChangedEvent,
}: {
  node: WorkspaceNode;
  codexState: CodexState;
  projectLookup: Map<string, Project>;
  remoteLookup: Map<string, RemoteConfig>;
  paneLookup: Map<string, WorkspacePane>;
  sessionLookup: Map<string, Session>;
  activePaneId: string | null;
  isLoading: boolean;
  draftsBySessionId: Record<string, string>;
  draftAttachmentsBySessionId: Record<string, DraftImageAttachment[]>;
  sendingSessionIds: SessionFlagMap;
  stoppingSessionIds: SessionFlagMap;
  killingSessionIds: SessionFlagMap;
  updatingSessionIds: SessionFlagMap;
  refreshingSessionModelOptionIds: SessionFlagMap;
  sessionModelOptionErrors: SessionErrorMap;
  agentCommandsBySessionId: SessionAgentCommandMap;
  refreshingAgentCommandSessionIds: SessionFlagMap;
  agentCommandErrors: SessionErrorMap;
  sessionSettingNotices: SessionNoticeMap;
  paneShouldStickToBottomRef: React.MutableRefObject<
    Record<string, boolean | undefined>
  >;
  paneScrollPositionsRef: React.MutableRefObject<
    Record<string, Record<string, { top: number; shouldStick: boolean }>>
  >;
  paneContentSignaturesRef: React.MutableRefObject<
    Record<string, Record<string, string>>
  >;
  pendingScrollToBottomRequest: {
    sessionId: string;
    token: number;
  } | null;
  windowId: string;
  draggedTab: WorkspaceTabDrag | null;
  getKnownDraggedTab: () => WorkspaceTabDrag | null;
  editorAppearance: MonacoAppearance;
  editorFontSizePx: number;
  onActivatePane: (paneId: string) => void;
  onSelectTab: (paneId: string, tabId: string) => void;
  onCloseTab: (paneId: string, tabId: string) => void;
  onSplitPane: (paneId: string, direction: "row" | "column") => void;
  onResizeStart: (
    splitId: string,
    direction: "row" | "column",
    event: ReactPointerEvent<HTMLDivElement>,
  ) => void;
  onTabDragStart: (drag: WorkspaceTabDrag) => void;
  onTabDragEnd: () => void;
  onTabDrop: (
    targetPaneId: string,
    placement: TabDropPlacement,
    tabIndex?: number,
    dataTransfer?: DataTransfer | null,
  ) => void;
  onPaneViewModeChange: (paneId: string, viewMode: SessionPaneViewMode) => void;
  onOpenSourceTab: (
    paneId: string,
    path: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: { line?: number; column?: number; openInNewTab?: boolean },
  ) => void;
  onOpenDiffPreviewTab: (
    paneId: string,
    message: DiffMessage,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenGitStatusDiffPreviewTab: (
    paneId: string,
    request: GitDiffRequestPayload,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: { openInNewTab?: boolean; sectionId?: GitDiffSection },
  ) => Promise<void> | void;
  onOpenFilesystemTab: (
    paneId: string,
    rootPath: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenGitStatusTab: (
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenInstructionDebuggerTab: (
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenCanvasTab: (
    paneId: string,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onUpsertCanvasSessionCard: (
    canvasTabId: string,
    sessionId: string,
    position: { x: number; y: number },
  ) => void;
  onRemoveCanvasSessionCard: (canvasTabId: string, sessionId: string) => void;
  onSetCanvasZoom: (canvasTabId: string, zoom: number) => void;
  onPaneSourcePathChange: (paneId: string, path: string) => void;
  onOpenConversationFromDiff: (
    sessionId: string,
    preferredPaneId: string | null,
  ) => void;
  onInsertReviewIntoPrompt: (
    sessionId: string,
    preferredPaneId: string | null,
    prompt: string,
  ) => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentsAdd: (
    sessionId: string,
    attachments: DraftImageAttachment[],
  ) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onComposerError: (message: string | null) => void;
  onSend: (
    sessionId: string,
    draftText?: string,
    expandedText?: string | null,
  ) => boolean;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
  onUserInputSubmit: (
    sessionId: string,
    messageId: string,
    answers: Record<string, string[]>,
  ) => void;
  onMcpElicitationSubmit: (
    sessionId: string,
    messageId: string,
    action: McpElicitationAction,
    content?: JsonValue,
  ) => void;
  onCodexAppRequestSubmit: (
    sessionId: string,
    messageId: string,
    result: JsonValue,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onKillSession: (sessionId: string) => void;
  onRenameSessionRequest: (
    sessionId: string,
    clientX: number,
    clientY: number,
    trigger?: HTMLElement | null,
  ) => void;
  onScrollToBottomRequestHandled: (token: number) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  onArchiveCodexThread: (sessionId: string) => void;
  onCompactCodexThread: (sessionId: string) => void;
  onForkCodexThread: (
    sessionId: string,
    preferredPaneId: string | null,
  ) => void;
  onRefreshSessionModelOptions: (sessionId: string) => void;
  onRefreshAgentCommands: (sessionId: string) => void;
  onRollbackCodexThread: (sessionId: string, numTurns: number) => void;
  onUnarchiveCodexThread: (sessionId: string) => void;
  onOrchestratorStateUpdated: (state: StateResponse) => void;
  renderControlPanel: (
    paneId: string,
    fixedSection?: ControlPanelSectionId | null,
  ) => JSX.Element;
  renderControlPanelPaneBarStatus: () => JSX.Element | null;
  renderControlPanelPaneBarActions: () => JSX.Element;
  backendConnectionState: BackendConnectionState;
  workspaceFilesChangedEvent: WorkspaceFilesChangedEvent | null;
}) {
  if (node.type === "pane") {
    const pane = paneLookup.get(node.paneId);
    if (!pane) {
      return null;
    }

    return (
      <SessionPaneView
        pane={pane}
        codexState={codexState}
        projectLookup={projectLookup}
        remoteLookup={remoteLookup}
        sessionLookup={sessionLookup}
        isActive={pane.id === activePaneId}
        isLoading={isLoading}
        draft={
          pane.activeSessionId
            ? (draftsBySessionId[pane.activeSessionId] ?? "")
            : ""
        }
        draftAttachments={
          pane.activeSessionId
            ? (draftAttachmentsBySessionId[pane.activeSessionId] ?? [])
            : []
        }
        isSending={
          pane.activeSessionId
            ? Boolean(sendingSessionIds[pane.activeSessionId])
            : false
        }
        isStopping={
          pane.activeSessionId
            ? Boolean(stoppingSessionIds[pane.activeSessionId])
            : false
        }
        isKilling={
          pane.activeSessionId
            ? Boolean(killingSessionIds[pane.activeSessionId])
            : false
        }
        isUpdating={
          pane.activeSessionId
            ? Boolean(updatingSessionIds[pane.activeSessionId])
            : false
        }
        isRefreshingModelOptions={
          pane.activeSessionId
            ? Boolean(refreshingSessionModelOptionIds[pane.activeSessionId])
            : false
        }
        modelOptionsError={
          pane.activeSessionId
            ? (sessionModelOptionErrors[pane.activeSessionId] ?? null)
            : null
        }
        agentCommands={
          pane.activeSessionId &&
          Object.prototype.hasOwnProperty.call(
            agentCommandsBySessionId,
            pane.activeSessionId,
          )
            ? (agentCommandsBySessionId[pane.activeSessionId] ?? [])
            : []
        }
        hasLoadedAgentCommands={
          pane.activeSessionId
            ? Object.prototype.hasOwnProperty.call(
                agentCommandsBySessionId,
                pane.activeSessionId,
              )
            : false
        }
        isRefreshingAgentCommands={
          pane.activeSessionId
            ? Boolean(refreshingAgentCommandSessionIds[pane.activeSessionId])
            : false
        }
        agentCommandsError={
          pane.activeSessionId
            ? (agentCommandErrors[pane.activeSessionId] ?? null)
            : null
        }
        sessionSettingNotice={
          pane.activeSessionId
            ? (sessionSettingNotices[pane.activeSessionId] ?? null)
            : null
        }
        paneShouldStickToBottomRef={paneShouldStickToBottomRef}
        paneScrollPositionsRef={paneScrollPositionsRef}
        paneContentSignaturesRef={paneContentSignaturesRef}
        pendingScrollToBottomRequest={pendingScrollToBottomRequest}
        windowId={windowId}
        draggedTab={draggedTab}
        getKnownDraggedTab={getKnownDraggedTab}
        editorAppearance={editorAppearance}
        editorFontSizePx={editorFontSizePx}
        onActivatePane={onActivatePane}
        onSelectTab={onSelectTab}
        onCloseTab={onCloseTab}
        onSplitPane={onSplitPane}
        onTabDragStart={onTabDragStart}
        onTabDragEnd={onTabDragEnd}
        onTabDrop={onTabDrop}
        onPaneViewModeChange={onPaneViewModeChange}
        onOpenSourceTab={onOpenSourceTab}
        onOpenDiffPreviewTab={onOpenDiffPreviewTab}
        onOpenGitStatusDiffPreviewTab={onOpenGitStatusDiffPreviewTab}
        onOpenFilesystemTab={onOpenFilesystemTab}
        onOpenGitStatusTab={onOpenGitStatusTab}
        onOpenInstructionDebuggerTab={onOpenInstructionDebuggerTab}
        onOpenCanvasTab={onOpenCanvasTab}
        onUpsertCanvasSessionCard={onUpsertCanvasSessionCard}
        onRemoveCanvasSessionCard={onRemoveCanvasSessionCard}
        onSetCanvasZoom={onSetCanvasZoom}
        onPaneSourcePathChange={onPaneSourcePathChange}
        onOpenConversationFromDiff={onOpenConversationFromDiff}
        onInsertReviewIntoPrompt={onInsertReviewIntoPrompt}
        onDraftCommit={onDraftCommit}
        onDraftAttachmentsAdd={onDraftAttachmentsAdd}
        onDraftAttachmentRemove={onDraftAttachmentRemove}
        onComposerError={onComposerError}
        onSend={onSend}
        onCancelQueuedPrompt={onCancelQueuedPrompt}
        onApprovalDecision={onApprovalDecision}
        onUserInputSubmit={onUserInputSubmit}
        onMcpElicitationSubmit={onMcpElicitationSubmit}
        onCodexAppRequestSubmit={onCodexAppRequestSubmit}
        onStopSession={onStopSession}
        onKillSession={onKillSession}
        onRenameSessionRequest={onRenameSessionRequest}
        onScrollToBottomRequestHandled={onScrollToBottomRequestHandled}
        onSessionSettingsChange={onSessionSettingsChange}
        onArchiveCodexThread={onArchiveCodexThread}
        onCompactCodexThread={onCompactCodexThread}
        onForkCodexThread={onForkCodexThread}
        onRefreshSessionModelOptions={onRefreshSessionModelOptions}
        onRefreshAgentCommands={onRefreshAgentCommands}
        onRollbackCodexThread={onRollbackCodexThread}
        onUnarchiveCodexThread={onUnarchiveCodexThread}
        onOrchestratorStateUpdated={onOrchestratorStateUpdated}
        renderControlPanel={renderControlPanel}
        renderControlPanelPaneBarStatus={renderControlPanelPaneBarStatus}
        renderControlPanelPaneBarActions={renderControlPanelPaneBarActions}
        backendConnectionState={backendConnectionState}
        workspaceFilesChangedEvent={workspaceFilesChangedEvent}
      />
    );
  }

  const firstContainsControlPanel = workspaceNodeContainsControlPanel(
    node.first,
    paneLookup,
  );
  const secondContainsControlPanel = workspaceNodeContainsControlPanel(
    node.second,
    paneLookup,
  );
  const firstUsesStandaloneControlSurfaceMinWidth =
    workspaceNodeUsesStandaloneControlSurfaceMinWidth(node.first, paneLookup);
  const secondUsesStandaloneControlSurfaceMinWidth =
    workspaceNodeUsesStandaloneControlSurfaceMinWidth(node.second, paneLookup);
  const branchClassName = (
    containsControlPanel: boolean,
    usesStandaloneControlSurfaceMinWidth: boolean,
  ) =>
    [
      "tile-branch",
      node.direction === "row" && containsControlPanel
        ? "control-panel-branch"
        : null,
      node.direction === "row" && usesStandaloneControlSurfaceMinWidth
        ? "standalone-control-surface-branch"
        : null,
    ]
      .filter(Boolean)
      .join(" ");
  const firstBranchClassName = branchClassName(
    firstContainsControlPanel,
    firstUsesStandaloneControlSurfaceMinWidth,
  );
  const secondBranchClassName = branchClassName(
    secondContainsControlPanel,
    secondUsesStandaloneControlSurfaceMinWidth,
  );

  return (
    <div className={`tile-split tile-split-${node.direction}`}>
      <div
        className={firstBranchClassName}
        style={{ flexGrow: node.ratio, flexBasis: 0 }}
      >
        <WorkspaceNodeView
          node={node.first}
          codexState={codexState}
          projectLookup={projectLookup}
          remoteLookup={remoteLookup}
          paneLookup={paneLookup}
          sessionLookup={sessionLookup}
          activePaneId={activePaneId}
          isLoading={isLoading}
          draftsBySessionId={draftsBySessionId}
          draftAttachmentsBySessionId={draftAttachmentsBySessionId}
          sendingSessionIds={sendingSessionIds}
          stoppingSessionIds={stoppingSessionIds}
          killingSessionIds={killingSessionIds}
          updatingSessionIds={updatingSessionIds}
          refreshingSessionModelOptionIds={refreshingSessionModelOptionIds}
          sessionModelOptionErrors={sessionModelOptionErrors}
          agentCommandsBySessionId={agentCommandsBySessionId}
          refreshingAgentCommandSessionIds={refreshingAgentCommandSessionIds}
          agentCommandErrors={agentCommandErrors}
          sessionSettingNotices={sessionSettingNotices}
          paneShouldStickToBottomRef={paneShouldStickToBottomRef}
          paneScrollPositionsRef={paneScrollPositionsRef}
          paneContentSignaturesRef={paneContentSignaturesRef}
          pendingScrollToBottomRequest={pendingScrollToBottomRequest}
          windowId={windowId}
          draggedTab={draggedTab}
          getKnownDraggedTab={getKnownDraggedTab}
          editorAppearance={editorAppearance}
          editorFontSizePx={editorFontSizePx}
          onActivatePane={onActivatePane}
          onSelectTab={onSelectTab}
          onCloseTab={onCloseTab}
          onSplitPane={onSplitPane}
          onResizeStart={onResizeStart}
          onTabDragStart={onTabDragStart}
          onTabDragEnd={onTabDragEnd}
          onTabDrop={onTabDrop}
          onPaneViewModeChange={onPaneViewModeChange}
          onOpenSourceTab={onOpenSourceTab}
          onOpenDiffPreviewTab={onOpenDiffPreviewTab}
          onOpenGitStatusDiffPreviewTab={onOpenGitStatusDiffPreviewTab}
          onOpenFilesystemTab={onOpenFilesystemTab}
          onOpenGitStatusTab={onOpenGitStatusTab}
          onOpenInstructionDebuggerTab={onOpenInstructionDebuggerTab}
          onOpenCanvasTab={onOpenCanvasTab}
          onUpsertCanvasSessionCard={onUpsertCanvasSessionCard}
          onRemoveCanvasSessionCard={onRemoveCanvasSessionCard}
          onSetCanvasZoom={onSetCanvasZoom}
          onPaneSourcePathChange={onPaneSourcePathChange}
          onOpenConversationFromDiff={onOpenConversationFromDiff}
          onInsertReviewIntoPrompt={onInsertReviewIntoPrompt}
          onDraftCommit={onDraftCommit}
          onDraftAttachmentsAdd={onDraftAttachmentsAdd}
          onDraftAttachmentRemove={onDraftAttachmentRemove}
          onComposerError={onComposerError}
          onSend={onSend}
          onCancelQueuedPrompt={onCancelQueuedPrompt}
          onApprovalDecision={onApprovalDecision}
          onUserInputSubmit={onUserInputSubmit}
          onMcpElicitationSubmit={onMcpElicitationSubmit}
          onCodexAppRequestSubmit={onCodexAppRequestSubmit}
          onStopSession={onStopSession}
          onKillSession={onKillSession}
          onRenameSessionRequest={onRenameSessionRequest}
          onScrollToBottomRequestHandled={onScrollToBottomRequestHandled}
          onSessionSettingsChange={onSessionSettingsChange}
          onArchiveCodexThread={onArchiveCodexThread}
          onCompactCodexThread={onCompactCodexThread}
          onForkCodexThread={onForkCodexThread}
          onRefreshSessionModelOptions={onRefreshSessionModelOptions}
          onRefreshAgentCommands={onRefreshAgentCommands}
          onRollbackCodexThread={onRollbackCodexThread}
          onUnarchiveCodexThread={onUnarchiveCodexThread}
          onOrchestratorStateUpdated={onOrchestratorStateUpdated}
          renderControlPanel={renderControlPanel}
          renderControlPanelPaneBarStatus={renderControlPanelPaneBarStatus}
          renderControlPanelPaneBarActions={renderControlPanelPaneBarActions}
          workspaceFilesChangedEvent={workspaceFilesChangedEvent}
          backendConnectionState={backendConnectionState}
        />
      </div>

      <div
        className={`tile-divider tile-divider-${node.direction}`}
        onPointerDown={(event) => onResizeStart(node.id, node.direction, event)}
      />

      <div
        className={secondBranchClassName}
        style={{ flexGrow: 1 - node.ratio, flexBasis: 0 }}
      >
        <WorkspaceNodeView
          node={node.second}
          codexState={codexState}
          projectLookup={projectLookup}
          remoteLookup={remoteLookup}
          paneLookup={paneLookup}
          sessionLookup={sessionLookup}
          activePaneId={activePaneId}
          isLoading={isLoading}
          draftsBySessionId={draftsBySessionId}
          draftAttachmentsBySessionId={draftAttachmentsBySessionId}
          sendingSessionIds={sendingSessionIds}
          stoppingSessionIds={stoppingSessionIds}
          killingSessionIds={killingSessionIds}
          updatingSessionIds={updatingSessionIds}
          refreshingSessionModelOptionIds={refreshingSessionModelOptionIds}
          sessionModelOptionErrors={sessionModelOptionErrors}
          agentCommandsBySessionId={agentCommandsBySessionId}
          refreshingAgentCommandSessionIds={refreshingAgentCommandSessionIds}
          agentCommandErrors={agentCommandErrors}
          sessionSettingNotices={sessionSettingNotices}
          paneShouldStickToBottomRef={paneShouldStickToBottomRef}
          paneScrollPositionsRef={paneScrollPositionsRef}
          paneContentSignaturesRef={paneContentSignaturesRef}
          pendingScrollToBottomRequest={pendingScrollToBottomRequest}
          windowId={windowId}
          draggedTab={draggedTab}
          getKnownDraggedTab={getKnownDraggedTab}
          editorAppearance={editorAppearance}
          editorFontSizePx={editorFontSizePx}
          onActivatePane={onActivatePane}
          onSelectTab={onSelectTab}
          onCloseTab={onCloseTab}
          onSplitPane={onSplitPane}
          onResizeStart={onResizeStart}
          onTabDragStart={onTabDragStart}
          onTabDragEnd={onTabDragEnd}
          onTabDrop={onTabDrop}
          onPaneViewModeChange={onPaneViewModeChange}
          onOpenSourceTab={onOpenSourceTab}
          onOpenDiffPreviewTab={onOpenDiffPreviewTab}
          onOpenGitStatusDiffPreviewTab={onOpenGitStatusDiffPreviewTab}
          onOpenFilesystemTab={onOpenFilesystemTab}
          onOpenGitStatusTab={onOpenGitStatusTab}
          onOpenInstructionDebuggerTab={onOpenInstructionDebuggerTab}
          onOpenCanvasTab={onOpenCanvasTab}
          onUpsertCanvasSessionCard={onUpsertCanvasSessionCard}
          onRemoveCanvasSessionCard={onRemoveCanvasSessionCard}
          onSetCanvasZoom={onSetCanvasZoom}
          onPaneSourcePathChange={onPaneSourcePathChange}
          onOpenConversationFromDiff={onOpenConversationFromDiff}
          onInsertReviewIntoPrompt={onInsertReviewIntoPrompt}
          onDraftCommit={onDraftCommit}
          onDraftAttachmentsAdd={onDraftAttachmentsAdd}
          onDraftAttachmentRemove={onDraftAttachmentRemove}
          onComposerError={onComposerError}
          onSend={onSend}
          onCancelQueuedPrompt={onCancelQueuedPrompt}
          onApprovalDecision={onApprovalDecision}
          onUserInputSubmit={onUserInputSubmit}
          onMcpElicitationSubmit={onMcpElicitationSubmit}
          onCodexAppRequestSubmit={onCodexAppRequestSubmit}
          onStopSession={onStopSession}
          onKillSession={onKillSession}
          onRenameSessionRequest={onRenameSessionRequest}
          onScrollToBottomRequestHandled={onScrollToBottomRequestHandled}
          onSessionSettingsChange={onSessionSettingsChange}
          onArchiveCodexThread={onArchiveCodexThread}
          onCompactCodexThread={onCompactCodexThread}
          onForkCodexThread={onForkCodexThread}
          onRefreshSessionModelOptions={onRefreshSessionModelOptions}
          onRefreshAgentCommands={onRefreshAgentCommands}
          onRollbackCodexThread={onRollbackCodexThread}
          onUnarchiveCodexThread={onUnarchiveCodexThread}
          onOrchestratorStateUpdated={onOrchestratorStateUpdated}
          renderControlPanel={renderControlPanel}
          renderControlPanelPaneBarStatus={renderControlPanelPaneBarStatus}
          renderControlPanelPaneBarActions={renderControlPanelPaneBarActions}
          workspaceFilesChangedEvent={workspaceFilesChangedEvent}
          backendConnectionState={backendConnectionState}
        />
      </div>
    </div>
  );
}

function SessionPaneView({
  pane,
  codexState,
  projectLookup,
  remoteLookup,
  sessionLookup,
  isActive,
  isLoading,
  draft,
  draftAttachments,
  isSending,
  isStopping,
  isKilling,
  isUpdating,
  isRefreshingModelOptions,
  modelOptionsError,
  agentCommands,
  hasLoadedAgentCommands,
  isRefreshingAgentCommands,
  agentCommandsError,
  sessionSettingNotice,
  paneShouldStickToBottomRef,
  paneScrollPositionsRef,
  paneContentSignaturesRef,
  pendingScrollToBottomRequest,
  windowId,
  draggedTab,
  getKnownDraggedTab,
  editorAppearance,
  editorFontSizePx,
  onActivatePane,
  onSelectTab,
  onCloseTab,
  onSplitPane,
  onTabDragStart,
  onTabDragEnd,
  onTabDrop,
  onPaneViewModeChange,
  onOpenSourceTab,
  onOpenDiffPreviewTab,
  onOpenGitStatusDiffPreviewTab,
  onOpenFilesystemTab,
  onOpenGitStatusTab,
  onOpenInstructionDebuggerTab,
  onOpenCanvasTab,
  onUpsertCanvasSessionCard,
  onRemoveCanvasSessionCard,
  onSetCanvasZoom,
  onPaneSourcePathChange,
  onOpenConversationFromDiff,
  onInsertReviewIntoPrompt,
  onDraftCommit,
  onDraftAttachmentsAdd,
  onDraftAttachmentRemove,
  onComposerError,
  onSend,
  onCancelQueuedPrompt,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onStopSession,
  onKillSession,
  onRenameSessionRequest,
  onScrollToBottomRequestHandled,
  onSessionSettingsChange,
  onArchiveCodexThread,
  onCompactCodexThread,
  onForkCodexThread,
  onRefreshSessionModelOptions,
  onRefreshAgentCommands,
  onRollbackCodexThread,
  onUnarchiveCodexThread,
  onOrchestratorStateUpdated,
  renderControlPanel,
  renderControlPanelPaneBarStatus,
  renderControlPanelPaneBarActions,
  workspaceFilesChangedEvent,
  backendConnectionState,
}: {
  pane: WorkspacePane;
  codexState: CodexState;
  projectLookup: Map<string, Project>;
  remoteLookup: Map<string, RemoteConfig>;
  sessionLookup: Map<string, Session>;
  isActive: boolean;
  isLoading: boolean;
  draft: string;
  draftAttachments: DraftImageAttachment[];
  isSending: boolean;
  isStopping: boolean;
  isKilling: boolean;
  isUpdating: boolean;
  isRefreshingModelOptions: boolean;
  modelOptionsError: string | null;
  agentCommands: AgentCommand[];
  hasLoadedAgentCommands: boolean;
  isRefreshingAgentCommands: boolean;
  agentCommandsError: string | null;
  sessionSettingNotice: string | null;
  paneShouldStickToBottomRef: React.MutableRefObject<
    Record<string, boolean | undefined>
  >;
  paneScrollPositionsRef: React.MutableRefObject<
    Record<string, Record<string, { top: number; shouldStick: boolean }>>
  >;
  paneContentSignaturesRef: React.MutableRefObject<
    Record<string, Record<string, string>>
  >;
  pendingScrollToBottomRequest: {
    sessionId: string;
    token: number;
  } | null;
  windowId: string;
  draggedTab: WorkspaceTabDrag | null;
  getKnownDraggedTab: () => WorkspaceTabDrag | null;
  editorAppearance: MonacoAppearance;
  editorFontSizePx: number;
  onActivatePane: (paneId: string) => void;
  onSelectTab: (paneId: string, tabId: string) => void;
  onCloseTab: (paneId: string, tabId: string) => void;
  onSplitPane: (paneId: string, direction: "row" | "column") => void;
  onTabDragStart: (drag: WorkspaceTabDrag) => void;
  onTabDragEnd: () => void;
  onTabDrop: (
    targetPaneId: string,
    placement: TabDropPlacement,
    tabIndex?: number,
    dataTransfer?: DataTransfer | null,
  ) => void;
  onPaneViewModeChange: (paneId: string, viewMode: SessionPaneViewMode) => void;
  onOpenSourceTab: (
    paneId: string,
    path: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: { line?: number; column?: number; openInNewTab?: boolean },
  ) => void;
  onOpenDiffPreviewTab: (
    paneId: string,
    message: DiffMessage,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenGitStatusDiffPreviewTab: (
    paneId: string,
    request: GitDiffRequestPayload,
    originSessionId: string | null,
    originProjectId: string | null,
    options?: { openInNewTab?: boolean; sectionId?: GitDiffSection },
  ) => Promise<void> | void;
  onOpenFilesystemTab: (
    paneId: string,
    rootPath: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenGitStatusTab: (
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenInstructionDebuggerTab: (
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onOpenCanvasTab: (
    paneId: string,
    originSessionId: string | null,
    originProjectId: string | null,
  ) => void;
  onUpsertCanvasSessionCard: (
    canvasTabId: string,
    sessionId: string,
    position: { x: number; y: number },
  ) => void;
  onRemoveCanvasSessionCard: (canvasTabId: string, sessionId: string) => void;
  onSetCanvasZoom: (canvasTabId: string, zoom: number) => void;
  onPaneSourcePathChange: (paneId: string, path: string) => void;
  onOpenConversationFromDiff: (
    sessionId: string,
    preferredPaneId: string | null,
  ) => void;
  onInsertReviewIntoPrompt: (
    sessionId: string,
    preferredPaneId: string | null,
    prompt: string,
  ) => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentsAdd: (
    sessionId: string,
    attachments: DraftImageAttachment[],
  ) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onComposerError: (message: string | null) => void;
  onSend: (
    sessionId: string,
    draftText?: string,
    expandedText?: string | null,
  ) => boolean;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
  onUserInputSubmit: (
    sessionId: string,
    messageId: string,
    answers: Record<string, string[]>,
  ) => void;
  onMcpElicitationSubmit: (
    sessionId: string,
    messageId: string,
    action: McpElicitationAction,
    content?: JsonValue,
  ) => void;
  onCodexAppRequestSubmit: (
    sessionId: string,
    messageId: string,
    result: JsonValue,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onKillSession: (sessionId: string) => void;
  onRenameSessionRequest: (
    sessionId: string,
    clientX: number,
    clientY: number,
    trigger?: HTMLElement | null,
  ) => void;
  onScrollToBottomRequestHandled: (token: number) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  onArchiveCodexThread: (sessionId: string) => void;
  onCompactCodexThread: (sessionId: string) => void;
  onForkCodexThread: (
    sessionId: string,
    preferredPaneId: string | null,
  ) => void;
  onRefreshSessionModelOptions: (sessionId: string) => void;
  onRefreshAgentCommands: (sessionId: string) => void;
  onRollbackCodexThread: (sessionId: string, numTurns: number) => void;
  onUnarchiveCodexThread: (sessionId: string) => void;
  onOrchestratorStateUpdated: (state: StateResponse) => void;
  renderControlPanel: (
    paneId: string,
    fixedSection?: ControlPanelSectionId | null,
  ) => JSX.Element;
  renderControlPanelPaneBarStatus: () => JSX.Element | null;
  renderControlPanelPaneBarActions: () => JSX.Element;
  workspaceFilesChangedEvent: WorkspaceFilesChangedEvent | null;
  backendConnectionState: BackendConnectionState;
}) {
  const activeTab =
    pane.tabs.find((tab) => tab.id === pane.activeTabId) ??
    pane.tabs[0] ??
    null;
  const activeControlPanelTab =
    activeTab?.kind === "controlPanel" ? activeTab : null;
  const activeOrchestratorListTab =
    activeTab?.kind === "orchestratorList" ? activeTab : null;
  const activeSessionListTab =
    activeTab?.kind === "sessionList" ? activeTab : null;
  const activeProjectListTab =
    activeTab?.kind === "projectList" ? activeTab : null;
  const activeCanvasTab = activeTab?.kind === "canvas" ? activeTab : null;
  const activeOrchestratorCanvasTab =
    activeTab?.kind === "orchestratorCanvas" ? activeTab : null;
  const activeControlSurfaceTab =
    activeControlPanelTab ??
    activeOrchestratorListTab ??
    activeSessionListTab ??
    activeProjectListTab;
  const activeSourceTab = activeTab?.kind === "source" ? activeTab : null;
  const activeFilesystemTab =
    activeTab?.kind === "filesystem" ? activeTab : null;
  const activeGitStatusTab = activeTab?.kind === "gitStatus" ? activeTab : null;
  const activeInstructionDebuggerTab =
    activeTab?.kind === "instructionDebugger" ? activeTab : null;
  const activeDiffPreviewTab =
    activeTab?.kind === "diffPreview" ? activeTab : null;
  const activeSourceOriginSessionId = activeSourceTab?.originSessionId ?? null;
  const activeSourceOriginProjectId = activeSourceTab?.originProjectId ?? null;
  const activeFilesystemOriginSessionId =
    activeFilesystemTab?.originSessionId ?? null;
  const activeFilesystemOriginProjectId =
    activeFilesystemTab?.originProjectId ?? null;
  const activeGitStatusOriginSessionId =
    activeGitStatusTab?.originSessionId ?? null;
  const activeGitStatusOriginProjectId =
    activeGitStatusTab?.originProjectId ?? null;
  const activeInstructionDebuggerOriginSessionId =
    activeInstructionDebuggerTab?.originSessionId ?? null;
  const activeInstructionDebuggerOriginProjectId =
    activeInstructionDebuggerTab?.originProjectId ?? null;
  const activeInstructionDebuggerSession =
    activeInstructionDebuggerOriginSessionId
      ? (sessionLookup.get(activeInstructionDebuggerOriginSessionId) ?? null)
      : null;
  const activeDiffOriginSessionId =
    activeDiffPreviewTab?.originSessionId ?? null;
  const activeDiffOriginProjectId =
    activeDiffPreviewTab?.originProjectId ?? null;
  const activeDiffWorkspaceRoot =
    (activeDiffOriginSessionId
      ? (sessionLookup.get(activeDiffOriginSessionId)?.workdir ?? null)
      : null) ??
    (activeDiffOriginProjectId
      ? (projectLookup.get(activeDiffOriginProjectId)?.rootPath ?? null)
      : null);
  const activeSourceWorkspaceRoot =
    (activeSourceOriginSessionId
      ? (sessionLookup.get(activeSourceOriginSessionId)?.workdir ?? null)
      : null) ??
    (activeSourceOriginProjectId
      ? (projectLookup.get(activeSourceOriginProjectId)?.rootPath ?? null)
      : null);
  const isSessionTabActive = activeTab?.kind === "session";
  const sessionTabs = useMemo(
    () =>
      pane.tabs.flatMap((tab) => {
        if (tab.kind !== "session") {
          return [];
        }

        const session = sessionLookup.get(tab.sessionId);
        return session ? [{ tab, session }] : [];
      }),
    [pane.tabs, sessionLookup],
  );
  const activeSession =
    (pane.activeSessionId ? sessionLookup.get(pane.activeSessionId) : null) ??
    sessionTabs[0]?.session ??
    null;
  const allKnownSessions = useMemo(
    () => Array.from(sessionLookup.values()),
    [sessionLookup],
  );
  const workspaceProjectOptions = useMemo<readonly ComboboxOption[]>(
    () =>
      Array.from(projectLookup.values()).map((project) => ({
        label: project.name,
        value: project.id,
        description: project.rootPath,
      })),
    [projectLookup],
  );
  const sessions = useMemo(
    () => sessionTabs.map(({ session }) => session),
    [sessionTabs],
  );
  const activeFilesystemScopeProjectId = activeFilesystemTab
    ? resolveWorkspaceScopedProjectId(
        activeFilesystemOriginProjectId,
        activeFilesystemOriginSessionId,
        sessionLookup,
        projectLookup,
      )
    : null;
  const activeGitScopeProjectId = activeGitStatusTab
    ? resolveWorkspaceScopedProjectId(
        activeGitStatusOriginProjectId,
        activeGitStatusOriginSessionId,
        sessionLookup,
        projectLookup,
      )
    : null;
  const activeFilesystemScopedSessionId = activeFilesystemScopeProjectId
    ? resolveWorkspaceScopedSessionId(
        activeFilesystemScopeProjectId,
        activeFilesystemOriginSessionId,
        activeSession,
        allKnownSessions,
        sessionLookup,
      )
    : activeFilesystemOriginSessionId;
  const activeGitScopedSessionId = activeGitScopeProjectId
    ? resolveWorkspaceScopedSessionId(
        activeGitScopeProjectId,
        activeGitStatusOriginSessionId,
        activeSession,
        allKnownSessions,
        sessionLookup,
      )
    : activeGitStatusOriginSessionId;
  const activeFilesystemScopedRootPath =
    activeFilesystemTab?.rootPath ??
    (activeFilesystemScopeProjectId
      ? resolveControlPanelWorkspaceRoot(
          projectLookup.get(activeFilesystemScopeProjectId) ?? null,
          null,
        )
      : null);
  const activeGitScopedWorkdir =
    activeGitStatusTab?.workdir ??
    (activeGitScopeProjectId
      ? resolveControlPanelWorkspaceRoot(
          projectLookup.get(activeGitScopeProjectId) ?? null,
          null,
        )
      : null);
  const shouldRenderFilesystemProjectScope =
    !!activeFilesystemScopeProjectId && workspaceProjectOptions.length > 0;
  const shouldRenderGitProjectScope =
    !!activeGitScopeProjectId && workspaceProjectOptions.length > 0;
  const [fileState, setFileState] = useState<SourceFileState>({
    status: "idle",
    path: "",
    content: "",
    contentHash: null,
    mtimeMs: null,
    sizeBytes: null,
    staleOnDisk: false,
    externalChangeKind: null,
    externalContentHash: null,
    externalMtimeMs: null,
    externalSizeBytes: null,
    error: null,
    language: null,
  });
  const [sourceEditorDirty, setSourceEditorDirty] = useState(false);
  const fileStateRef = useRef(fileState);
  const sourceEditorDirtyRef = useRef(false);
  const messageStackRef = useRef<HTMLElement | null>(null);
  const paneTopRef = useRef<HTMLDivElement | null>(null);
  const [activeDropPlacement, setActiveDropPlacement] = useState<Exclude<
    TabDropPlacement,
    "tabs"
  > | null>(null);
  const [pointerDraggedTab, setPointerDraggedTab] =
    useState<WorkspaceTabDrag | null>(null);
  const [visitedSessionIds, setVisitedSessionIds] = useState<
    Record<string, true | undefined>
  >({});
  const [cachedSessionOrder, setCachedSessionOrder] = useState<string[]>([]);
  const [newResponseIndicatorByKey, setNewResponseIndicatorByKey] = useState<
    Record<string, true | undefined>
  >({});

  useEffect(() => {
    fileStateRef.current = fileState;
  }, [fileState]);

  function renderWorkspaceTabProjectScope(
    scopeId: string,
    value: string,
    onChange: (nextValue: string) => void,
  ) {
    return (
      <div className="control-panel-scope-control">
        <label className="control-panel-scope-label" htmlFor={scopeId}>
          Project
        </label>
        <ThemedCombobox
          id={scopeId}
          className="control-panel-scope-combobox"
          value={value}
          options={workspaceProjectOptions}
          onChange={onChange}
          aria-label="Project"
        />
      </div>
    );
  }
  const [isSessionFindOpen, setIsSessionFindOpen] = useState(false);
  const [sessionFindQuery, setSessionFindQuery] = useState("");
  const [sessionFindActiveIndex, setSessionFindActiveIndex] = useState(0);
  const sessionFindInputRef = useRef<HTMLInputElement>(null);
  const sessionSearchItemRefsRef = useRef<Record<string, HTMLElement | null>>(
    {},
  );
  const paneHasControlPanel = useMemo(
    () => pane.tabs.some((tab) => tab.kind === "controlPanel"),
    [pane.tabs],
  );
  const effectiveDraggedTab = draggedTab ?? pointerDraggedTab;
  const allowedDropPlacements = useMemo<Exclude<TabDropPlacement, "tabs">[]>(
    () =>
      effectiveDraggedTab &&
      (effectiveDraggedTab.tab.kind === "controlPanel" || paneHasControlPanel)
        ? ["left", "right"]
        : ["left", "top", "right", "bottom"],
    [effectiveDraggedTab, paneHasControlPanel],
  );
  const showDropOverlay =
    Boolean(effectiveDraggedTab) &&
    !(
      effectiveDraggedTab?.sourceWindowId === windowId &&
      effectiveDraggedTab?.sourcePaneId === pane.id &&
      pane.tabs.length <= 1
    ) &&
    !(activeCanvasTab && effectiveDraggedTab?.tab.kind === "session");
  const sourceCandidatePaths = useMemo(
    () =>
      activeSourceTab && activeSession
        ? collectCandidateSourcePaths(activeSession)
        : [],
    [activeSession, activeSourceTab],
  );
  const commandMessages = useMemo(
    () =>
      pane.viewMode === "commands" && activeSession
        ? activeSession.messages.filter(
            (message): message is CommandMessage => message.type === "command",
          )
        : [],
    [activeSession, pane.viewMode],
  );
  const diffMessages = useMemo(
    () =>
      pane.viewMode === "diffs" && activeSession
        ? activeSession.messages.filter(
            (message): message is DiffMessage => message.type === "diff",
          )
        : [],
    [activeSession, pane.viewMode],
  );
  const pendingPrompts = useMemo(
    () => activeSession?.pendingPrompts ?? [],
    [activeSession],
  );
  const mountedSessionsRef = useRef<Session[]>([]);
  const mountedSessions = useMemo(() => {
    if (!activeSession) {
      if (mountedSessionsRef.current.length === 0) {
        return mountedSessionsRef.current;
      }
      mountedSessionsRef.current = [];
      return mountedSessionsRef.current;
    }

    const cachedSessionIds = new Set(cachedSessionOrder);
    cachedSessionIds.add(activeSession.id);
    const next = sessions.filter((session) => cachedSessionIds.has(session.id));
    const prev = mountedSessionsRef.current;
    if (
      next.length === prev.length &&
      next.every((session, index) => session === prev[index])
    ) {
      return prev;
    }
    mountedSessionsRef.current = next;
    return next;
  }, [activeSession, cachedSessionOrder, sessions]);
  const sessionConversationSignature = useMemo(
    () =>
      pane.viewMode === "session" && activeSession
        ? buildSessionConversationSignature(activeSession)
        : "",
    [activeSession, pane.viewMode],
  );
  const isSessionBusy =
    activeSession?.status === "active" || activeSession?.status === "approval";
  const showWaitingIndicator =
    isSessionTabActive &&
    pane.viewMode === "session" &&
    Boolean(activeSession) &&
    (activeSession?.status === "active" || (!isSessionBusy && isSending));
  const canFindInSession =
    isSessionTabActive && pane.viewMode === "session" && Boolean(activeSession);
  const hasSessionFindQuery =
    canFindInSession && sessionFindQuery.trim().length > 0;
  const activeSessionFindSearchIndex = useMemo(
    () =>
      canFindInSession && hasSessionFindQuery && activeSession
        ? buildSessionSearchIndex(activeSession)
        : null,
    [activeSession, canFindInSession, hasSessionFindQuery],
  );
  const sessionSearchMatches = useMemo(
    () =>
      activeSessionFindSearchIndex
        ? buildSessionSearchMatchesFromIndex(
            activeSessionFindSearchIndex,
            sessionFindQuery,
          )
        : [],
    [activeSessionFindSearchIndex, sessionFindQuery],
  );
  const sessionSearchMatchedItemKeys = useMemo(
    () => new Set(sessionSearchMatches.map((match) => match.itemKey)),
    [sessionSearchMatches],
  );
  const activeSessionSearchMatch =
    sessionSearchMatches.length > 0
      ? (sessionSearchMatches[
          Math.min(sessionFindActiveIndex, sessionSearchMatches.length - 1)
        ] ?? null)
      : null;
  const activeSessionSearchMatchIndex = activeSessionSearchMatch
    ? Math.min(sessionFindActiveIndex, sessionSearchMatches.length - 1)
    : -1;
  const waitingIndicatorPrompt = useMemo(() => {
    if (
      !showWaitingIndicator ||
      !activeSession ||
      (!isSessionBusy && isSending)
    ) {
      return null;
    }

    return findLastUserPrompt(activeSession);
  }, [activeSession, isSending, isSessionBusy, showWaitingIndicator]);
  const composerInputDisabled = !activeSession || isStopping;
  const composerSendDisabled = !activeSession || isSending || isStopping;
  const scrollStateKey = activeSourceTab
    ? `${pane.id}:source:${activeSourceTab.path ?? "empty"}`
    : activeCanvasTab
      ? `${pane.id}:canvas:${activeCanvasTab.id}`
      : activeOrchestratorCanvasTab
        ? `${pane.id}:orchestratorCanvas:${activeOrchestratorCanvasTab.id}`
        : activeFilesystemTab
          ? `${pane.id}:filesystem:${activeFilesystemTab.rootPath ?? "empty"}`
          : activeGitStatusTab
            ? `${pane.id}:gitStatus:${activeGitStatusTab.workdir ?? "empty"}`
            : activeInstructionDebuggerTab
              ? `${pane.id}:instructionDebugger:${activeInstructionDebuggerTab.originSessionId ?? activeInstructionDebuggerTab.workdir ?? "empty"}`
              : activeDiffPreviewTab
                ? `${pane.id}:diffPreview:${activeDiffPreviewTab.diffMessageId}`
                : `${pane.id}:${pane.viewMode}:${activeSession?.id ?? "empty"}`;
  const defaultScrollToBottom =
    pane.viewMode === "session" ||
    pane.viewMode === "commands" ||
    pane.viewMode === "diffs";
  const visibleMessages = useMemo(
    () =>
      pane.viewMode === "commands"
        ? commandMessages
        : pane.viewMode === "diffs"
          ? diffMessages
          : [],
    [commandMessages, diffMessages, pane.viewMode],
  );
  const visibleContentSignature = useMemo(
    () =>
      pane.viewMode === "session"
        ? sessionConversationSignature
        : buildMessageListSignature(visibleMessages),
    [pane.viewMode, sessionConversationSignature, visibleMessages],
  );
  const visibleLastMessageAuthor = useMemo(
    () =>
      pane.viewMode === "session"
        ? activeSession?.messages[activeSession.messages.length - 1]?.author
        : visibleMessages[visibleMessages.length - 1]?.author,
    [activeSession, pane.viewMode, visibleMessages],
  );
  const showNewResponseIndicator = Boolean(
    newResponseIndicatorByKey[scrollStateKey],
  );
  const paneScrollPositions =
    paneScrollPositionsRef.current[pane.id] ??
    (paneScrollPositionsRef.current[pane.id] = {});
  const paneContentSignatures =
    paneContentSignaturesRef.current[pane.id] ??
    (paneContentSignaturesRef.current[pane.id] = {});

  function getShouldStickToBottom() {
    return paneShouldStickToBottomRef.current[pane.id] ?? true;
  }

  function setShouldStickToBottom(nextValue: boolean) {
    paneShouldStickToBottomRef.current[pane.id] = nextValue;
  }

  function handleComposerPaste(
    event: ReactClipboardEvent<HTMLTextAreaElement>,
  ) {
    if (!activeSession) {
      return;
    }

    const imageFiles = collectClipboardImageFiles(event.clipboardData);
    if (imageFiles.length === 0) {
      return;
    }

    event.preventDefault();

    void createDraftAttachmentsFromFiles(imageFiles)
      .then(({ attachments, errors }) => {
        if (attachments.length > 0) {
          onDraftAttachmentsAdd(activeSession.id, attachments);
        }

        if (errors.length > 0) {
          onComposerError(errors[0]);
        } else {
          onComposerError(null);
        }
      })
      .catch((error) => {
        onComposerError(getErrorMessage(error));
      });
  }

  async function handleSourceFileSave(
    path: string,
    content: string,
    sessionId: string | null,
    projectId: string | null,
    options?: SourceSaveOptions,
  ) {
    if (!sessionId && !projectId) {
      throw new Error(
        "This file view is no longer associated with a live session or project.",
      );
    }

    const response = await saveFile(path, content, {
      sessionId,
      projectId,
      baseHash:
        options?.baseHash !== undefined
          ? options.baseHash
          : fileState.status === "ready" && fileState.path === path
            ? fileState.contentHash
            : null,
      overwrite: options?.overwrite,
    });
    sourceEditorDirtyRef.current = false;
    setSourceEditorDirty(false);
    setFileState(sourceFileStateFromResponse(response));
    return response;
  }

  async function handleSourceFileReload(
    path: string,
    sessionId: string | null,
    projectId: string | null,
  ) {
    const nextFileState = await handleSourceFileFetchLatest(
      path,
      sessionId,
      projectId,
    );
    sourceEditorDirtyRef.current = false;
    setSourceEditorDirty(false);
    setFileState(nextFileState);
  }

  async function handleSourceFileFetchLatest(
    path: string,
    sessionId: string | null,
    projectId: string | null,
  ) {
    if (!sessionId && !projectId) {
      throw new Error(
        "This file view is no longer associated with a live session or project.",
      );
    }

    const response = await fetchFile(path, {
      sessionId,
      projectId,
    });
    return sourceFileStateFromResponse(response);
  }

  function handleSourceFileAdopt(nextFileState: SourceFileState) {
    setFileState(nextFileState);
  }

  function handleSourceEditorDirtyChange(isDirty: boolean) {
    sourceEditorDirtyRef.current = isDirty;
    setSourceEditorDirty(isDirty);
  }

  function setNewResponseIndicator(key: string, visible: boolean) {
    setNewResponseIndicatorByKey((current) => {
      const isVisible = Boolean(current[key]);
      if (isVisible === visible) {
        return current;
      }

      const nextState = { ...current };
      if (visible) {
        nextState[key] = true;
      } else {
        delete nextState[key];
      }
      return nextState;
    });
  }

  function handleConversationSearchItemMount(
    itemKey: string,
    node: HTMLElement | null,
  ) {
    if (node) {
      sessionSearchItemRefsRef.current[itemKey] = node;
      return;
    }

    delete sessionSearchItemRefsRef.current[itemKey];
  }

  function focusSessionFindInput(selectAll = false) {
    window.requestAnimationFrame(() => {
      const input = sessionFindInputRef.current;
      if (!input) {
        return;
      }

      input.focus();
      if (selectAll) {
        input.select();
      }
    });
  }

  function openSessionFind(selectAll = true) {
    if (!canFindInSession) {
      return;
    }

    setIsSessionFindOpen(true);
    focusSessionFindInput(selectAll);
  }

  function closeSessionFind() {
    setIsSessionFindOpen(false);
    setSessionFindQuery("");
    setSessionFindActiveIndex(0);
    sessionSearchItemRefsRef.current = {};
  }

  function stepSessionFind(direction: -1 | 1) {
    if (sessionSearchMatches.length === 0) {
      return;
    }

    setSessionFindActiveIndex((current) => {
      const safeCurrent =
        current >= 0 && current < sessionSearchMatches.length ? current : 0;
      return (
        (safeCurrent + direction + sessionSearchMatches.length) %
        sessionSearchMatches.length
      );
    });
  }

  function scrollToLatestMessage(behavior: ScrollBehavior) {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    node.scrollTo({
      top: node.scrollHeight,
      behavior,
    });
    setShouldStickToBottom(true);
    paneScrollPositions[scrollStateKey] = {
      top: node.scrollHeight,
      shouldStick: true,
    };
    setNewResponseIndicator(scrollStateKey, false);
  }

  function scrollMessageStackByPage(direction: -1 | 1) {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    const distance = Math.max(Math.round(node.clientHeight * 0.85), 160);
    node.scrollBy({
      top: distance * direction,
      behavior: "smooth",
    });
  }

  function scrollMessageStackToBoundary(boundary: "top" | "bottom") {
    if (boundary === "bottom") {
      scheduleSettledScrollToBottom("auto", { maxAttempts: 60 });
      setShouldStickToBottom(true);
      paneScrollPositions[scrollStateKey] = {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      };
      setNewResponseIndicator(scrollStateKey, false);
      return;
    }

    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    node.scrollTo({
      top: 0,
      behavior: "auto",
    });
    setShouldStickToBottom(false);
    paneScrollPositions[scrollStateKey] = {
      top: 0,
      shouldStick: false,
    };
  }

  function handlePaneKeyDown(event: ReactKeyboardEvent<HTMLElement>) {
    if (event.defaultPrevented) {
      return;
    }

    const command = resolvePaneScrollCommand(
      {
        altKey: event.altKey,
        ctrlKey: event.ctrlKey,
        key: event.key,
        metaKey: event.metaKey,
        shiftKey: event.shiftKey,
      },
      event.target,
    );
    if (!command) {
      return;
    }

    event.preventDefault();
    if (command.kind === "boundary") {
      scrollMessageStackToBoundary(
        command.direction === "up" ? "top" : "bottom",
      );
    } else {
      scrollMessageStackByPage(command.direction === "up" ? -1 : 1);
    }
  }

  function handleMessageStackWheel(event: ReactWheelEvent<HTMLElement>) {
    if (event.defaultPrevented || event.ctrlKey) {
      return;
    }

    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    const deltaY = normalizeWheelDelta(event, node);
    if (Math.abs(deltaY) < 0.5) {
      return;
    }

    if (canNestedScrollableConsumeWheel(event.target, node, deltaY)) {
      return;
    }

    const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    if (maxScrollTop <= 0) {
      return;
    }

    const nextScrollTop = clamp(node.scrollTop + deltaY, 0, maxScrollTop);
    if (Math.abs(nextScrollTop - node.scrollTop) < 0.5) {
      return;
    }

    event.preventDefault();
    node.scrollTop = nextScrollTop;
    const { shouldStick } = syncMessageStackScrollPosition(
      node,
      scrollStateKey,
      paneScrollPositions,
    );
    setShouldStickToBottom(shouldStick);
    if (shouldStick) {
      setNewResponseIndicator(scrollStateKey, false);
    }
  }

  useEffect(() => {
    if (canFindInSession) {
      return;
    }

    closeSessionFind();
  }, [canFindInSession]);

  useEffect(() => {
    closeSessionFind();
  }, [activeSession?.id]);

  useEffect(() => {
    setSessionFindActiveIndex(0);
  }, [sessionFindQuery]);

  useEffect(() => {
    if (!isActive || !canFindInSession) {
      return;
    }

    function handleWindowKeyDown(event: KeyboardEvent) {
      const key = event.key.toLowerCase();
      const hasPrimaryModifier = event.metaKey || event.ctrlKey;
      if (
        event.defaultPrevented ||
        key !== "f" ||
        !hasPrimaryModifier ||
        event.altKey ||
        event.shiftKey
      ) {
        return;
      }

      event.preventDefault();
      openSessionFind();
    }

    window.addEventListener("keydown", handleWindowKeyDown);
    return () => {
      window.removeEventListener("keydown", handleWindowKeyDown);
    };
  }, [canFindInSession, isActive]);

  function scheduleSettledScrollToBottom(
    behavior: ScrollBehavior,
    options: {
      maxAttempts?: number;
      onComplete?: () => void;
    } = {},
  ) {
    let frameId = 0;
    let cancelled = false;
    let completed = false;
    let remainingAttempts = options.maxAttempts ?? 12;
    let previousScrollHeight = -1;
    let stableFrameCount = 0;

    function complete() {
      if (cancelled || completed) {
        return;
      }

      completed = true;
      options.onComplete?.();
    }

    const tick = () => {
      const node = messageStackRef.current;
      if (!node) {
        remainingAttempts -= 1;
        if (remainingAttempts > 0) {
          frameId = window.requestAnimationFrame(tick);
        } else {
          complete();
        }
        return;
      }

      scrollToLatestMessage(behavior);

      const bottomGap = Math.max(
        node.scrollHeight - node.clientHeight - node.scrollTop,
        0,
      );
      const heightStable = node.scrollHeight === previousScrollHeight;
      if (bottomGap <= 4 && heightStable) {
        stableFrameCount += 1;
      } else {
        stableFrameCount = 0;
      }

      previousScrollHeight = node.scrollHeight;
      remainingAttempts -= 1;
      if (remainingAttempts > 0 && stableFrameCount < 2) {
        frameId = window.requestAnimationFrame(tick);
      } else {
        complete();
      }
    };

    frameId = window.requestAnimationFrame(tick);
    return () => {
      cancelled = true;
      window.cancelAnimationFrame(frameId);
    };
  }

  useLayoutEffect(() => {
    const frameId = window.requestAnimationFrame(() => {
      const node = messageStackRef.current;
      if (!node) {
        return;
      }

      const saved = paneScrollPositions[scrollStateKey];
      if (saved) {
        // TODO: restore the exact saved scroll offset on tab switch.
        // The virtualizer recalculates layout from estimated heights on
        // remount, so the saved pixel offset no longer maps to the same
        // messages. A proper fix needs to save the first-visible message
        // ID and scroll to its position in the new layout. For now,
        // always scroll to the bottom — this is correct for the common
        // case (user was at the bottom chatting) and acceptable for the
        // scrolled-up case (user loses their position but can scroll back).
        scheduleSettledScrollToBottom("auto", { maxAttempts: 60 });
        setShouldStickToBottom(saved.shouldStick);
        return;
      }

      if (defaultScrollToBottom) {
        scheduleSettledScrollToBottom("auto", { maxAttempts: 60 });
        setShouldStickToBottom(true);
        paneScrollPositions[scrollStateKey] = {
          top: Number.MAX_SAFE_INTEGER,
          shouldStick: true,
        };
        return;
      }

      node.scrollTop = 0;
      setShouldStickToBottom(false);
      paneScrollPositions[scrollStateKey] = {
        top: 0,
        shouldStick: false,
      };
    });

    return () => {
      window.cancelAnimationFrame(frameId);
    };
  }, [defaultScrollToBottom, scrollStateKey]);

  useLayoutEffect(() => {
    if (!hasSessionFindQuery || !activeSessionSearchMatch) {
      return;
    }

    const node =
      sessionSearchItemRefsRef.current[activeSessionSearchMatch.itemKey];
    if (!node) {
      return;
    }

    setShouldStickToBottom(false);
    node.scrollIntoView({
      block: "center",
      behavior: "auto",
    });

    const container = messageStackRef.current;
    if (!container) {
      return;
    }

    paneScrollPositions[scrollStateKey] = {
      top: container.scrollTop,
      shouldStick: false,
    };
    setNewResponseIndicator(scrollStateKey, false);
  }, [
    activeSessionSearchMatch,
    hasSessionFindQuery,
    paneScrollPositions,
    scrollStateKey,
  ]);

  useLayoutEffect(() => {
    if (
      !activeSession ||
      !isSessionTabActive ||
      pane.viewMode !== "session" ||
      visitedSessionIds[activeSession.id]
    ) {
      return;
    }

    return scheduleSettledScrollToBottom("auto");
  }, [
    activeSession,
    isSessionTabActive,
    pane.viewMode,
    scrollStateKey,
    visitedSessionIds,
  ]);

  useEffect(() => {
    if (!activeSession?.id) {
      return;
    }

    setVisitedSessionIds((current) =>
      current[activeSession.id]
        ? current
        : {
            ...current,
            [activeSession.id]: true,
          },
    );
    setCachedSessionOrder((current) => {
      const nextOrder = [
        activeSession.id,
        ...current.filter((sessionId) => sessionId !== activeSession.id),
      ].slice(0, MAX_CACHED_SESSION_PAGES_PER_PANE);

      if (
        nextOrder.length === current.length &&
        nextOrder.every((sessionId, index) => sessionId === current[index])
      ) {
        return current;
      }

      return nextOrder;
    });
  }, [activeSession?.id]);

  useEffect(() => {
    const availableSessionIds = new Set(sessions.map((session) => session.id));
    setVisitedSessionIds((current) =>
      pruneSessionFlags(current, availableSessionIds),
    );
    setCachedSessionOrder((current) => {
      const nextOrder = current.filter((sessionId) =>
        availableSessionIds.has(sessionId),
      );
      if (
        nextOrder.length === current.length &&
        nextOrder.every((sessionId, index) => sessionId === current[index])
      ) {
        return current;
      }

      return nextOrder;
    });
  }, [sessions]);

  useEffect(() => {
    if (!activeSession || !isSessionTabActive) {
      return;
    }

    const previousSignature = paneContentSignatures[scrollStateKey];
    paneContentSignatures[scrollStateKey] = visibleContentSignature;
    if (previousSignature === visibleContentSignature) {
      return;
    }
    if (previousSignature === undefined) {
      // First content after mount. The useLayoutEffect already tried to
      // scroll, but messages may not have been available yet (SSE loads
      // state asynchronously). If the initial intent was to stick to the
      // bottom, honour it now that content has arrived.
      if (getShouldStickToBottom()) {
        scheduleSettledScrollToBottom("auto", { maxAttempts: 60 });
      }
      return;
    }

    if (hasSessionFindQuery) {
      setShouldStickToBottom(false);
      if (
        pane.viewMode === "session" &&
        visibleLastMessageAuthor === "assistant"
      ) {
        setNewResponseIndicator(scrollStateKey, true);
      }
      return;
    }

    const shouldScroll =
      getShouldStickToBottom() ||
      paneScrollPositions[scrollStateKey]?.shouldStick === true ||
      visibleLastMessageAuthor === "you";
    if (!shouldScroll) {
      if (
        pane.viewMode === "session" &&
        visibleLastMessageAuthor === "assistant"
      ) {
        setNewResponseIndicator(scrollStateKey, true);
      }
      return;
    }

    const behavior = visibleLastMessageAuthor === "you" ? "smooth" : "auto";
    setNewResponseIndicator(scrollStateKey, false);
    const frameId = window.requestAnimationFrame(() => {
      scrollToLatestMessage(behavior);
    });

    return () => {
      window.cancelAnimationFrame(frameId);
    };
  }, [
    activeSession,
    hasSessionFindQuery,
    isSessionTabActive,
    pane.viewMode,
    scrollStateKey,
    visibleContentSignature,
    visibleLastMessageAuthor,
  ]);

  useEffect(() => {
    if (
      !pendingScrollToBottomRequest ||
      !isActive ||
      pane.viewMode !== "session" ||
      activeSession?.id !== pendingScrollToBottomRequest.sessionId
    ) {
      return;
    }

    const requestToken = pendingScrollToBottomRequest.token;
    return scheduleSettledScrollToBottom("auto", {
      onComplete: () => {
        onScrollToBottomRequestHandled(requestToken);
      },
    });
  }, [
    activeSession?.id,
    isActive,
    onScrollToBottomRequestHandled,
    pane.viewMode,
    pendingScrollToBottomRequest,
    scrollStateKey,
  ]);

  useEffect(() => {
    if (!isSending || pane.viewMode !== "session") {
      return;
    }

    const frameId = window.requestAnimationFrame(() => {
      scrollToLatestMessage("smooth");
    });

    return () => {
      window.cancelAnimationFrame(frameId);
    };
  }, [isSending, pane.viewMode, scrollStateKey]);

  useEffect(() => {
    if (pane.viewMode !== "source") {
      return;
    }

    if (!pane.sourcePath && sourceCandidatePaths[0]) {
      onPaneSourcePathChange(pane.id, sourceCandidatePaths[0]);
    }
  }, [
    onPaneSourcePathChange,
    pane.id,
    pane.sourcePath,
    pane.viewMode,
    sourceCandidatePaths,
  ]);

  useEffect(() => {
    let cancelled = false;

    async function loadFile(path: string) {
      if (!activeSourceOriginSessionId && !activeSourceOriginProjectId) {
        setFileState({
          status: "error",
          path,
          content: "",
          contentHash: null,
          mtimeMs: null,
          sizeBytes: null,
          staleOnDisk: false,
          externalChangeKind: null,
          externalContentHash: null,
          externalMtimeMs: null,
          externalSizeBytes: null,
          error:
            "This file view is no longer associated with a live session or project.",
          language: null,
        });
        sourceEditorDirtyRef.current = false;
        setSourceEditorDirty(false);
        return;
      }

      setFileState({
        status: "loading",
        path,
        content: "",
        contentHash: null,
        mtimeMs: null,
        sizeBytes: null,
        staleOnDisk: false,
        externalChangeKind: null,
        externalContentHash: null,
        externalMtimeMs: null,
        externalSizeBytes: null,
        error: null,
        language: null,
      });
      sourceEditorDirtyRef.current = false;
      setSourceEditorDirty(false);

      try {
        const response = await fetchFile(path, {
          sessionId: activeSourceOriginSessionId,
          projectId: activeSourceOriginProjectId,
        });
        if (cancelled) {
          return;
        }

        sourceEditorDirtyRef.current = false;
        setSourceEditorDirty(false);
        setFileState(sourceFileStateFromResponse(response));
      } catch (error) {
        if (cancelled) {
          return;
        }

        setFileState({
          status: "error",
          path,
          content: "",
          contentHash: null,
          mtimeMs: null,
          sizeBytes: null,
          staleOnDisk: false,
          externalChangeKind: null,
          externalContentHash: null,
          externalMtimeMs: null,
          externalSizeBytes: null,
          error: getErrorMessage(error),
          language: null,
        });
        sourceEditorDirtyRef.current = false;
        setSourceEditorDirty(false);
      }
    }

    if (pane.viewMode === "source" && pane.sourcePath) {
      void loadFile(pane.sourcePath);
    } else if (pane.viewMode === "source") {
      setFileState({
        status: "idle",
        path: "",
        content: "",
        contentHash: null,
        mtimeMs: null,
        sizeBytes: null,
        staleOnDisk: false,
        externalChangeKind: null,
        externalContentHash: null,
        externalMtimeMs: null,
        externalSizeBytes: null,
        error: null,
        language: null,
      });
      sourceEditorDirtyRef.current = false;
      setSourceEditorDirty(false);
    }

    return () => {
      cancelled = true;
    };
  }, [
    activeSourceOriginProjectId,
    activeSourceOriginSessionId,
    pane.sourcePath,
    pane.viewMode,
  ]);

  useEffect(() => {
    if (
      !workspaceFilesChangedEvent ||
      pane.viewMode !== "source" ||
      !pane.sourcePath ||
      (!activeSourceOriginSessionId && !activeSourceOriginProjectId)
    ) {
      return;
    }

    const fileChangeEvent = workspaceFilesChangedEvent;
    let cancelled = false;

    async function checkOpenSourceFile() {
      const current = fileStateRef.current;
      if (
        current.status !== "ready" ||
        current.path !== pane.sourcePath ||
        !current.contentHash
      ) {
        return;
      }

      const fileChange = workspaceFilesChangedEventChangeForPath(
        fileChangeEvent,
        current.path,
        {
          rootPath: activeSourceWorkspaceRoot,
          sessionId: activeSourceOriginSessionId,
        },
      );
      if (!fileChange) {
        return;
      }

      if (fileChange.kind === "deleted") {
        setFileState((latest) =>
          latest.status === "ready" &&
          latest.path === current.path &&
          latest.contentHash === current.contentHash
            ? {
                ...latest,
                staleOnDisk: true,
                externalChangeKind: "deleted",
                externalContentHash: null,
                externalMtimeMs: fileChange.mtimeMs ?? null,
                externalSizeBytes: fileChange.sizeBytes ?? null,
              }
            : latest,
        );
        return;
      }

      try {
        const response = await fetchFile(current.path, {
          sessionId: activeSourceOriginSessionId,
          projectId: activeSourceOriginProjectId,
        });
        if (cancelled) {
          return;
        }

        const nextHash = response.contentHash ?? null;
        if (!nextHash || nextHash === current.contentHash) {
          if (current.staleOnDisk) {
            setFileState((latest) =>
              latest.status === "ready" &&
              latest.path === current.path &&
              latest.contentHash === current.contentHash
                ? {
                    ...latest,
                    staleOnDisk: false,
                    externalChangeKind: null,
                    externalContentHash: null,
                    externalMtimeMs: null,
                    externalSizeBytes: null,
                  }
                : latest,
            );
          }
          return;
        }

        if (sourceEditorDirtyRef.current) {
          setFileState((latest) =>
            latest.status === "ready" &&
            latest.path === current.path &&
            latest.contentHash === current.contentHash
              ? {
                  ...latest,
                  staleOnDisk: true,
                  externalChangeKind: fileChange.kind,
                  externalContentHash: nextHash,
                  externalMtimeMs: response.mtimeMs ?? null,
                  externalSizeBytes: response.sizeBytes ?? null,
                }
              : latest,
          );
          return;
        }

        setFileState(sourceFileStateFromResponse(response));
      } catch (error) {
        if (!cancelled && isSourceFileMissingError(error)) {
          setFileState((latest) =>
            latest.status === "ready" &&
            latest.path === current.path &&
            latest.contentHash === current.contentHash
              ? {
                  ...latest,
                  staleOnDisk: true,
                  externalChangeKind: "deleted",
                  externalContentHash: null,
                  externalMtimeMs: fileChange.mtimeMs ?? null,
                  externalSizeBytes: fileChange.sizeBytes ?? null,
                }
              : latest,
          );
        }
        // Other transient read errors stay out of the editor. The explicit file
        // load and save paths still surface failures where the user can act.
      }
    }

    void checkOpenSourceFile();

    return () => {
      cancelled = true;
    };
  }, [
    activeSourceOriginProjectId,
    activeSourceOriginSessionId,
    activeSourceWorkspaceRoot,
    pane.sourcePath,
    pane.viewMode,
    workspaceFilesChangedEvent,
  ]);

  useEffect(() => {
    if (!showDropOverlay) {
      setActiveDropPlacement(null);
      setPointerDraggedTab(null);
    }
  }, [showDropOverlay]);

  const tabDecorations = useMemo<Record<string, PaneTabDecoration>>(() => {
    if (!activeSourceTab || fileState.status !== "ready") {
      return {};
    }

    let decoration: PaneTabDecoration | null = null;
    if (fileState.staleOnDisk && sourceEditorDirty) {
      decoration = {
        label: "Conflict",
        tone: "danger",
        title: "This file changed on disk while you have unsaved edits.",
      };
    } else if (fileState.staleOnDisk) {
      decoration = {
        label: "Changed",
        tone: "info",
        title: "This file changed on disk.",
      };
    } else if (sourceEditorDirty) {
      decoration = {
        label: "Unsaved",
        tone: "warning",
        title: "This file has unsaved editor changes.",
      };
    }

    return decoration ? { [activeSourceTab.id]: decoration } : {};
  }, [activeSourceTab, fileState, sourceEditorDirty]);

  return (
    <section
      className={`workspace-pane thread panel ${isActive ? "active" : ""}`}
      onMouseDown={() => {
        if (!isActive) {
          onActivatePane(pane.id);
        }
      }}
      onDragLeave={(event) => {
        const nextTarget = event.relatedTarget;
        if (
          nextTarget instanceof Node &&
          event.currentTarget.contains(nextTarget)
        ) {
          return;
        }

        setActiveDropPlacement(null);
        setPointerDraggedTab(null);
      }}
      onDragOver={(event) => {
        if (isPointerWithinPaneTopArea(paneTopRef.current, event.clientY)) {
          setActiveDropPlacement(null);
          return;
        }

        const knownWorkspaceTabDrag = pointerDraggedTab ?? getKnownDraggedTab();
        const currentDrag =
          knownWorkspaceTabDrag ?? readWorkspaceTabDragData(event.dataTransfer);
        const hasSessionDragType = dataTransferHasSessionDragType(
          event.dataTransfer,
        );
        const dragTypes = event.dataTransfer?.types;
        const hasKnownWorkspaceTabDrag = Boolean(knownWorkspaceTabDrag);
        const hasTabDragType = Boolean(
          dragTypes?.includes(TAB_DRAG_MIME_TYPE) ||
          (hasKnownWorkspaceTabDrag && dragTypes?.includes("text/plain")),
        );
        if (!currentDrag && !hasTabDragType && !hasSessionDragType) {
          return;
        }

        event.preventDefault();
        event.dataTransfer.dropEffect =
          hasSessionDragType ||
          currentDrag?.sourcePaneId.startsWith("control-panel-launcher:")
            ? "copy"
            : currentDrag
              ? "move"
              : "copy";

        if (!draggedTab && currentDrag) {
          setPointerDraggedTab((existing) =>
            existing?.dragId === currentDrag.dragId ? existing : currentDrag,
          );
        }

        const nextPlacement = resolvePaneDropPlacementFromPointer(
          event.currentTarget.getBoundingClientRect(),
          event.clientX,
          event.clientY,
          allowedDropPlacements,
        );
        setActiveDropPlacement((current) =>
          current === nextPlacement ? current : nextPlacement,
        );
      }}
      onDrop={(event) => {
        if (isPointerWithinPaneTopArea(paneTopRef.current, event.clientY)) {
          setActiveDropPlacement(null);
          setPointerDraggedTab(null);
          return;
        }

        const currentDrag =
          pointerDraggedTab ??
          getKnownDraggedTab() ??
          readWorkspaceTabDragData(event.dataTransfer);
        if (
          !currentDrag &&
          !dataTransferHasSessionDragType(event.dataTransfer)
        ) {
          return;
        }

        event.preventDefault();
        const nextPlacement =
          activeDropPlacement ??
          resolvePaneDropPlacementFromPointer(
            event.currentTarget.getBoundingClientRect(),
            event.clientX,
            event.clientY,
            allowedDropPlacements,
          );
        setActiveDropPlacement(null);
        setPointerDraggedTab(null);
        onTabDrop(pane.id, nextPlacement, undefined, event.dataTransfer);
      }}
      onKeyDown={handlePaneKeyDown}
    >
      {showDropOverlay ? (
        <div className="pane-drop-overlay">
          {allowedDropPlacements.map((placement) => (
            <div
              key={placement}
              className={`pane-drop-zone pane-drop-zone-${placement} ${activeDropPlacement === placement ? "active" : ""}`}
              onDragEnter={() => {
                setActiveDropPlacement(placement);
              }}
              onDragOver={(event) => {
                event.preventDefault();
                event.dataTransfer.dropEffect = "move";
                if (activeDropPlacement !== placement) {
                  setActiveDropPlacement(placement);
                }
              }}
              onDragLeave={() => {
                setActiveDropPlacement((current) =>
                  current === placement ? null : current,
                );
              }}
              onDrop={(event) => {
                event.preventDefault();
                event.stopPropagation();
                setActiveDropPlacement(null);
                setPointerDraggedTab(null);
                onTabDrop(pane.id, placement, undefined, event.dataTransfer);
              }}
            >
              <span>{dropLabelForPlacement(placement)}</span>
            </div>
          ))}
        </div>
      ) : null}

      <div ref={paneTopRef} className="pane-top">
        <div className="pane-bar">
          <div className="pane-bar-left">
            <PaneTabs
              paneId={pane.id}
              windowId={windowId}
              tabs={pane.tabs}
              activeTabId={activeTab?.id ?? null}
              codexState={codexState}
              projectLookup={projectLookup}
              remoteLookup={remoteLookup}
              sessionLookup={sessionLookup}
              draggedTab={draggedTab}
              getKnownDraggedTab={getKnownDraggedTab}
              tabDecorations={tabDecorations}
              onSelectTab={onSelectTab}
              onCloseTab={onCloseTab}
              onTabDragStart={onTabDragStart}
              onTabDragEnd={onTabDragEnd}
              onTabDrop={onTabDrop}
              onRenameSessionRequest={onRenameSessionRequest}
            />
            {activeControlSurfaceTab
              ? renderControlPanelPaneBarStatus()
              : null}
          </div>
          {activeTab?.kind === "controlPanel" ? (
            <div className="pane-bar-right">
              {renderControlPanelPaneBarActions()}
            </div>
          ) : null}
        </div>

        <div className="pane-view-strip">
          {activeTab?.kind === "session" ? (
            <div className="pane-view-strip-left">
              {(
                [
                  "session",
                  "prompt",
                  "commands",
                  "diffs",
                ] as SessionPaneViewMode[]
              ).map((viewMode) => (
                <button
                  key={viewMode}
                  className={`pane-view-button ${pane.viewMode === viewMode ? "selected" : ""}`}
                  type="button"
                  onClick={() => onPaneViewModeChange(pane.id, viewMode)}
                >
                  {labelForPaneViewMode(viewMode)}
                </button>
              ))}
              <button
                className="pane-view-button"
                type="button"
                onClick={() => {
                  const candidatePath = activeSession
                    ? (collectCandidateSourcePaths(activeSession)[0] ?? null)
                    : null;
                  onOpenSourceTab(
                    pane.id,
                    candidatePath,
                    activeSession?.id ?? null,
                    activeSession?.projectId ?? null,
                  );
                }}
              >
                File
              </button>
              <button
                className="pane-view-button"
                type="button"
                onClick={() =>
                  onOpenFilesystemTab(
                    pane.id,
                    activeSession?.workdir ?? null,
                    activeSession?.id ?? null,
                    activeSession?.projectId ?? null,
                  )
                }
              >
                Files
              </button>
              <button
                className="pane-view-button"
                type="button"
                onClick={() =>
                  onOpenGitStatusTab(
                    pane.id,
                    activeSession?.workdir ?? null,
                    activeSession?.id ?? null,
                    activeSession?.projectId ?? null,
                  )
                }
              >
                Git
              </button>
              <button
                className="pane-view-button"
                type="button"
                onClick={() =>
                  onOpenInstructionDebuggerTab(
                    pane.id,
                    activeSession?.workdir ?? null,
                    activeSession?.id ?? null,
                    activeSession?.projectId ?? null,
                  )
                }
              >
                Instructions
              </button>
              <button
                className="pane-view-button"
                type="button"
                onClick={() =>
                  onOpenCanvasTab(
                    pane.id,
                    activeSession?.id ?? null,
                    activeSession?.projectId ?? null,
                  )
                }
              >
                Canvas
              </button>
              {canFindInSession ? (
                <button
                  className={`pane-view-button${isSessionFindOpen ? " selected" : ""}`}
                  type="button"
                  onClick={() => openSessionFind(!isSessionFindOpen)}
                  title={`Find in session (${primaryModifierLabel()}+F)`}
                >
                  Find
                </button>
              ) : null}
            </div>
          ) : null}
          <div className="pane-view-strip-right">
            {canFindInSession && isSessionFindOpen ? (
              <SessionFindBar
                inputRef={sessionFindInputRef}
                query={sessionFindQuery}
                activeIndex={activeSessionSearchMatchIndex}
                matches={sessionSearchMatches}
                onChange={(nextValue) => setSessionFindQuery(nextValue)}
                onNext={() => stepSessionFind(1)}
                onPrevious={() => stepSessionFind(-1)}
                onClose={closeSessionFind}
              />
            ) : null}
          </div>
        </div>
      </div>

      <section
        ref={messageStackRef}
        className={`message-stack${activeControlSurfaceTab || activeOrchestratorCanvasTab ? " control-panel-stack" : ""}${activeSourceTab || activeDiffPreviewTab ? " editor-panel-stack" : ""}`}
        onWheel={handleMessageStackWheel}
        onScroll={(event) => {
          const node = event.currentTarget;
          const { shouldStick } = syncMessageStackScrollPosition(
            node,
            scrollStateKey,
            paneScrollPositions,
          );
          setShouldStickToBottom(shouldStick);
          if (shouldStick) {
            setNewResponseIndicator(scrollStateKey, false);
          }
        }}
      >
        {activeControlPanelTab ? (
          renderControlPanel(pane.id)
        ) : activeOrchestratorListTab ? (
          renderControlPanel(pane.id, "orchestrators")
        ) : activeSessionListTab ? (
          renderControlPanel(pane.id, "sessions")
        ) : activeProjectListTab ? (
          renderControlPanel(pane.id, "projects")
        ) : activeCanvasTab ? (
          <SessionCanvasPanel
            tab={activeCanvasTab}
            sessionLookup={sessionLookup}
            draggedTab={draggedTab}
            onOpenSession={(sessionId) =>
              onOpenConversationFromDiff(sessionId, pane.id)
            }
            onRemoveCard={(sessionId) =>
              onRemoveCanvasSessionCard(activeCanvasTab.id, sessionId)
            }
            onSetZoom={(zoom) => onSetCanvasZoom(activeCanvasTab.id, zoom)}
            onUpsertCard={(sessionId, position) =>
              onUpsertCanvasSessionCard(activeCanvasTab.id, sessionId, position)
            }
          />
        ) : activeOrchestratorCanvasTab ? (
          <OrchestratorTemplatesPanel
            initialTemplateId={activeOrchestratorCanvasTab.templateId ?? null}
            persistenceKey={activeOrchestratorCanvasTab.id}
            projects={Array.from(projectLookup.values())}
            sessions={allKnownSessions}
            onStateUpdated={onOrchestratorStateUpdated}
            startMode={
              activeOrchestratorCanvasTab.startMode === "new"
                ? "new"
                : activeOrchestratorCanvasTab.templateId
                  ? "edit"
                  : "browse"
            }
          />
        ) : activeSourceTab ? (
          <SourcePanel
            editorAppearance={editorAppearance}
            editorFontSizePx={editorFontSizePx}
            fileState={fileState}
            sourceFocus={
              activeSourceTab?.focusLineNumber
                ? {
                    line: activeSourceTab.focusLineNumber,
                    column: activeSourceTab.focusColumnNumber ?? null,
                    token: activeSourceTab.focusToken ?? null,
                  }
                : null
            }
            sourcePath={activeSourceTab.path}
            onOpenInstructionDebugger={
              activeSourceOriginSessionId
                ? () =>
                    onOpenInstructionDebuggerTab(
                      pane.id,
                      sessionLookup.get(activeSourceOriginSessionId)?.workdir ??
                        null,
                      activeSourceOriginSessionId,
                      activeSourceOriginProjectId,
                    )
                : null
            }
            onDirtyChange={handleSourceEditorDirtyChange}
            onFetchLatestFile={(path) =>
              handleSourceFileFetchLatest(
                path,
                activeSourceOriginSessionId,
                activeSourceOriginProjectId,
              )
            }
            onAdoptFileState={handleSourceFileAdopt}
            onReloadFile={(path) =>
              handleSourceFileReload(
                path,
                activeSourceOriginSessionId,
                activeSourceOriginProjectId,
              )
            }
            onSaveFile={async (path, content, options) => {
              await handleSourceFileSave(
                path,
                content,
                activeSourceOriginSessionId,
                activeSourceOriginProjectId,
                options,
              );
            }}
          />
        ) : activeFilesystemTab ? (
          shouldRenderFilesystemProjectScope ? (
            <section
              className="control-panel-section-stack control-panel-section-files"
              aria-label="Files"
            >
              {renderWorkspaceTabProjectScope(
                `workspace-project-scope-${pane.id}-filesystem`,
                activeFilesystemScopeProjectId,
                (nextProjectId) => {
                  const nextProject = projectLookup.get(nextProjectId) ?? null;
                  if (!nextProject) {
                    return;
                  }

                  onOpenFilesystemTab(
                    pane.id,
                    resolveControlPanelWorkspaceRoot(nextProject, null),
                    resolveWorkspaceScopedSessionId(
                      nextProjectId,
                      null,
                      activeSession,
                      allKnownSessions,
                      sessionLookup,
                    ),
                    nextProject.id,
                  );
                },
              )}
              <FileSystemPanel
                rootPath={activeFilesystemScopedRootPath}
                sessionId={activeFilesystemScopedSessionId}
                projectId={activeFilesystemScopeProjectId}
                workspaceFilesChangedEvent={workspaceFilesChangedEvent}
                showPathControls={false}
                onOpenPath={(path, options) =>
                  onOpenSourceTab(
                    pane.id,
                    path,
                    activeFilesystemScopedSessionId,
                    activeFilesystemScopeProjectId,
                    options,
                  )
                }
                onOpenRootPath={(path) =>
                  onOpenFilesystemTab(
                    pane.id,
                    path,
                    activeFilesystemScopedSessionId,
                    activeFilesystemScopeProjectId,
                  )
                }
              />
            </section>
          ) : (
            <FileSystemPanel
              rootPath={activeFilesystemTab.rootPath}
              sessionId={activeFilesystemOriginSessionId}
              projectId={activeFilesystemOriginProjectId}
              workspaceFilesChangedEvent={workspaceFilesChangedEvent}
              onOpenPath={(path, options) =>
                onOpenSourceTab(
                  pane.id,
                  path,
                  activeFilesystemOriginSessionId,
                  activeFilesystemOriginProjectId,
                  options,
                )
              }
              onOpenRootPath={(path) =>
                onOpenFilesystemTab(
                  pane.id,
                  path,
                  activeFilesystemOriginSessionId,
                  activeFilesystemOriginProjectId,
                )
              }
            />
          )
        ) : activeGitStatusTab ? (
          shouldRenderGitProjectScope ? (
            <section
              className="control-panel-section-stack control-panel-section-git"
              aria-label="Git status"
            >
              {renderWorkspaceTabProjectScope(
                `workspace-project-scope-${pane.id}-git`,
                activeGitScopeProjectId,
                (nextProjectId) => {
                  const nextProject = projectLookup.get(nextProjectId) ?? null;
                  if (!nextProject) {
                    return;
                  }

                  onOpenGitStatusTab(
                    pane.id,
                    resolveControlPanelWorkspaceRoot(nextProject, null),
                    resolveWorkspaceScopedSessionId(
                      nextProjectId,
                      null,
                      activeSession,
                      allKnownSessions,
                      sessionLookup,
                    ),
                    nextProject.id,
                  );
                },
              )}
              <GitStatusPanel
                projectId={activeGitScopeProjectId}
                sessionId={activeGitScopedSessionId}
                showPathControls={false}
                workdir={activeGitScopedWorkdir}
                onOpenDiff={(diff, options) =>
                  onOpenGitStatusDiffPreviewTab(
                    pane.id,
                    diff,
                    activeGitScopedSessionId,
                    activeGitScopeProjectId,
                    options,
                  )
                }
                onOpenWorkdir={(path) =>
                  onOpenGitStatusTab(
                    pane.id,
                    path,
                    activeGitScopedSessionId,
                    activeGitScopeProjectId,
                  )
                }
              />
            </section>
          ) : (
            <GitStatusPanel
              projectId={activeGitStatusOriginProjectId}
              sessionId={activeGitStatusOriginSessionId}
              workdir={activeGitStatusTab.workdir}
              onOpenDiff={(diff, options) =>
                onOpenGitStatusDiffPreviewTab(
                  pane.id,
                  diff,
                  activeGitStatusOriginSessionId,
                  activeGitStatusOriginProjectId,
                  options,
                )
              }
              onOpenWorkdir={(path) =>
                onOpenGitStatusTab(
                  pane.id,
                  path,
                  activeGitStatusOriginSessionId,
                  activeGitStatusOriginProjectId,
                )
              }
            />
          )
        ) : activeInstructionDebuggerTab ? (
          <InstructionDebuggerPanel
            session={activeInstructionDebuggerSession}
            workdir={activeInstructionDebuggerTab.workdir}
            onOpenPath={(path, options) =>
              onOpenSourceTab(
                pane.id,
                path,
                activeInstructionDebuggerOriginSessionId,
                activeInstructionDebuggerOriginProjectId,
                options,
              )
            }
          />
        ) : activeDiffPreviewTab ? (
          activeDiffPreviewTab.isLoading || activeDiffPreviewTab.loadError ? (
            <div
              className={`source-editor-loading diff-preview-loading-state${activeDiffPreviewTab.loadError ? " is-error" : ""}`}
              role={activeDiffPreviewTab.isLoading ? "status" : "alert"}
              aria-live={activeDiffPreviewTab.isLoading ? "polite" : undefined}
            >
              <div className="diff-preview-loading-copy">
                {activeDiffPreviewTab.isLoading ? (
                  <span
                    className="activity-spinner diff-preview-loading-spinner"
                    aria-hidden="true"
                  />
                ) : null}
                <strong>
                  {activeDiffPreviewTab.isLoading
                    ? "Loading diff"
                    : "Unable to load diff"}
                </strong>
                {activeDiffPreviewTab.filePath ? (
                  <span className="diff-preview-loading-path">
                    {normalizeDisplayPath(activeDiffPreviewTab.filePath)}
                  </span>
                ) : null}
                <span className="diff-preview-loading-detail">
                  {activeDiffPreviewTab.loadError ??
                    "Fetching git diff from the repository..."}
                </span>
              </div>
            </div>
          ) : (
            <DiffPanel
              appearance={editorAppearance}
              changeType={activeDiffPreviewTab.changeType}
              changeSetId={activeDiffPreviewTab.changeSetId ?? null}
              fontSizePx={editorFontSizePx}
              diff={activeDiffPreviewTab.diff}
              diffMessageId={activeDiffPreviewTab.diffMessageId}
              filePath={activeDiffPreviewTab.filePath}
              gitSectionId={activeDiffPreviewTab.gitSectionId ?? null}
              language={activeDiffPreviewTab.language ?? null}
              sessionId={activeDiffOriginSessionId}
              projectId={activeDiffOriginProjectId}
              originAgentName={
                activeDiffOriginSessionId
                  ? (sessionLookup.get(activeDiffOriginSessionId)?.agent ??
                    null)
                  : null
              }
              workspaceRoot={activeDiffWorkspaceRoot}
              workspaceFilesChangedEvent={workspaceFilesChangedEvent}
              onOpenPath={(path) =>
                onOpenSourceTab(
                  pane.id,
                  path,
                  activeDiffOriginSessionId,
                  activeDiffOriginProjectId,
                )
              }
              onInsertReviewIntoPrompt={
                activeDiffOriginSessionId
                  ? (_reviewFilePath, prompt) =>
                      onInsertReviewIntoPrompt(
                        activeDiffOriginSessionId,
                        pane.id,
                        prompt,
                      )
                  : undefined
              }
              onOpenConversation={
                activeDiffOriginSessionId
                  ? () =>
                      onOpenConversationFromDiff(
                        activeDiffOriginSessionId,
                        pane.id,
                      )
                  : undefined
              }
              onSaveFile={(path, content) =>
                handleSourceFileSave(
                  path,
                  content,
                  activeDiffOriginSessionId,
                  activeDiffOriginProjectId,
                )
              }
              summary={activeDiffPreviewTab.summary}
            />
          )
        ) : (
          <AgentSessionPanel
            paneId={pane.id}
            viewMode={pane.viewMode}
            scrollContainerRef={messageStackRef}
            activeSession={activeSession}
            isLoading={isLoading}
            isUpdating={isUpdating}
            showWaitingIndicator={showWaitingIndicator}
            waitingIndicatorPrompt={waitingIndicatorPrompt}
            mountedSessions={mountedSessions}
            commandMessages={commandMessages}
            diffMessages={diffMessages}
            onApprovalDecision={onApprovalDecision}
            onUserInputSubmit={onUserInputSubmit}
            onMcpElicitationSubmit={onMcpElicitationSubmit}
            onCodexAppRequestSubmit={onCodexAppRequestSubmit}
            onCancelQueuedPrompt={onCancelQueuedPrompt}
            onSessionSettingsChange={onSessionSettingsChange}
            conversationSearchQuery={
              hasSessionFindQuery ? sessionFindQuery : ""
            }
            conversationSearchMatchedItemKeys={sessionSearchMatchedItemKeys}
            conversationSearchActiveItemKey={
              activeSessionSearchMatch?.itemKey ?? null
            }
            onConversationSearchItemMount={handleConversationSearchItemMount}
            renderCommandCard={(message) => <CommandCard message={message} />}
            renderDiffCard={(message) => (
              <DiffCard
                message={message}
                onOpenPreview={() =>
                  onOpenDiffPreviewTab(
                    pane.id,
                    message,
                    activeSession?.id ?? null,
                    activeSession?.projectId ?? null,
                  )
                }
                workspaceRoot={activeSession?.workdir ?? null}
              />
            )}
            renderMessageCard={(
              message,
              preferImmediateHeavyRender,
              handleDecision,
              handleUserInput,
              handleMcpElicitation,
              handleCodexAppRequest,
            ) => (
              <MessageCard
                message={message}
                onOpenDiffPreview={(diffMessage) =>
                  onOpenDiffPreviewTab(
                    pane.id,
                    diffMessage,
                    activeSession?.id ?? null,
                    activeSession?.projectId ?? null,
                  )
                }
                onOpenSourceLink={(target) =>
                  onOpenSourceTab(
                    pane.id,
                    target.path,
                    activeSession?.id ?? null,
                    activeSession?.projectId ?? null,
                    {
                      line: target.line,
                      column: target.column,
                      openInNewTab: target.openInNewTab,
                    },
                  )
                }
                preferImmediateHeavyRender={preferImmediateHeavyRender}
                onApprovalDecision={handleDecision}
                onUserInputSubmit={handleUserInput}
                onMcpElicitationSubmit={handleMcpElicitation}
                onCodexAppRequestSubmit={handleCodexAppRequest}
                searchQuery={
                  activeSessionSearchMatch?.itemKey === `message:${message.id}`
                    ? sessionFindQuery
                    : ""
                }
                searchHighlightTone={
                  activeSessionSearchMatch?.itemKey === `message:${message.id}`
                    ? "active"
                    : "match"
                }
                workspaceRoot={activeSession?.workdir ?? null}
              />
            )}
            renderPromptSettings={(
              panelPaneId,
              session,
              panelIsUpdating,
              handleSettingsChange,
            ) => {
              if (session.agent === "Codex") {
                return (
                  <CodexPromptSettingsCard
                    paneId={panelPaneId}
                    session={session}
                    isUpdating={panelIsUpdating}
                    isRefreshingModelOptions={isRefreshingModelOptions}
                    modelOptionsError={modelOptionsError}
                    sessionNotice={
                      session.id === activeSession?.id
                        ? sessionSettingNotice
                        : null
                    }
                    onRequestModelOptions={onRefreshSessionModelOptions}
                    onArchiveThread={onArchiveCodexThread}
                    onCompactThread={onCompactCodexThread}
                    onForkThread={onForkCodexThread}
                    onRollbackThread={onRollbackCodexThread}
                    onSessionSettingsChange={handleSettingsChange}
                    onUnarchiveThread={onUnarchiveCodexThread}
                  />
                );
              }

              if (session.agent === "Claude") {
                return (
                  <ClaudePromptSettingsCard
                    paneId={panelPaneId}
                    session={session}
                    isUpdating={panelIsUpdating}
                    isRefreshingModelOptions={isRefreshingModelOptions}
                    modelOptionsError={modelOptionsError}
                    onRequestModelOptions={onRefreshSessionModelOptions}
                    onSessionSettingsChange={handleSettingsChange}
                  />
                );
              }

              if (session.agent === "Cursor") {
                return (
                  <CursorPromptSettingsCard
                    paneId={panelPaneId}
                    session={session}
                    isUpdating={panelIsUpdating}
                    isRefreshingModelOptions={isRefreshingModelOptions}
                    modelOptionsError={modelOptionsError}
                    onRequestModelOptions={onRefreshSessionModelOptions}
                    onSessionSettingsChange={handleSettingsChange}
                  />
                );
              }

              if (session.agent === "Gemini") {
                return (
                  <GeminiPromptSettingsCard
                    paneId={panelPaneId}
                    session={session}
                    isUpdating={panelIsUpdating}
                    isRefreshingModelOptions={isRefreshingModelOptions}
                    modelOptionsError={modelOptionsError}
                    onRequestModelOptions={onRefreshSessionModelOptions}
                    onSessionSettingsChange={handleSettingsChange}
                  />
                );
              }

              return null;
            }}
          />
        )}
      </section>
      {activeControlSurfaceTab ||
      activeCanvasTab ||
      activeOrchestratorCanvasTab ||
      activeSourceTab ||
      activeFilesystemTab ||
      activeGitStatusTab ||
      activeInstructionDebuggerTab ||
      activeDiffPreviewTab ? null : (
        <AgentSessionPanelFooter
          paneId={pane.id}
          viewMode={pane.viewMode}
          isPaneActive={isActive}
          activeSession={activeSession}
          committedDraft={draft}
          draftAttachments={draftAttachments}
          formatByteSize={formatByteSize}
          isSending={isSending}
          isStopping={isStopping}
          isSessionBusy={isSessionBusy}
          isUpdating={isUpdating}
          showNewResponseIndicator={showNewResponseIndicator}
          footerModeLabel={labelForPaneViewMode(pane.viewMode)}
          onScrollToLatest={() => scrollToLatestMessage("smooth")}
          onDraftCommit={onDraftCommit}
          onDraftAttachmentRemove={onDraftAttachmentRemove}
          isRefreshingModelOptions={isRefreshingModelOptions}
          modelOptionsError={modelOptionsError}
          agentCommands={agentCommands}
          hasLoadedAgentCommands={hasLoadedAgentCommands}
          isRefreshingAgentCommands={isRefreshingAgentCommands}
          agentCommandsError={agentCommandsError}
          onRefreshSessionModelOptions={onRefreshSessionModelOptions}
          onRefreshAgentCommands={onRefreshAgentCommands}
          onSend={onSend}
          onSessionSettingsChange={onSessionSettingsChange}
          onStopSession={onStopSession}
          onPaste={handleComposerPaste}
        />
      )}
    </section>
  );
}

function EmptyState({ title, body }: { title: string; body: string }) {
  return (
    <article className="empty-state">
      <div className="card-label">Live State</div>
      <h3>{title}</h3>
      <p>{body}</p>
    </article>
  );
}

function workspaceNodeContainsControlPanel(
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

function getActiveWorkspacePaneTab(pane: WorkspacePane): WorkspaceTab | null {
  return (
    pane.tabs.find((tab) => tab.id === pane.activeTabId) ??
    pane.tabs[0] ??
    null
  );
}

function paneHasActiveStandaloneControlSurface(pane: WorkspacePane): boolean {
  const activeTab = getActiveWorkspacePaneTab(pane);
  return Boolean(
    activeTab &&
      activeTab.kind !== "controlPanel" &&
      CONTROL_SURFACE_KINDS.has(activeTab.kind),
  );
}

function workspaceNodeContainsStandaloneControlSurface(
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

function workspaceNodeContainsNonControlSurfacePane(
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

function workspaceNodeUsesStandaloneControlSurfaceMinWidth(
  node: WorkspaceNode,
  paneLookup: Map<string, WorkspacePane>,
): boolean {
  return (
    !workspaceNodeContainsControlPanel(node, paneLookup) &&
    !workspaceNodeContainsNonControlSurfacePane(node, paneLookup) &&
    workspaceNodeContainsStandaloneControlSurface(node, paneLookup)
  );
}

function findWorkspaceSplitNode(
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

function workspaceContainsOnlyControlPanel(workspace: WorkspaceState) {
  return (
    workspace.panes.length === 1 &&
    workspace.panes[0]?.tabs.length === 1 &&
    workspace.panes[0]?.tabs[0]?.kind === "controlPanel"
  );
}

export function resolveStandaloneControlPanelDockWidthRatio(
  fallbackRatio: number,
): number {
  if (typeof document === "undefined") {
    return fallbackRatio;
  }

  const workspaceStage =
    document.querySelector(
      ".workspace-stage.workspace-stage-control-panel-only",
    ) ?? document.querySelector(".workspace-stage");
  const stageWidth =
    workspaceStage instanceof HTMLElement && workspaceStage.clientWidth > 0
      ? workspaceStage.clientWidth
      : (document.documentElement?.clientWidth ??
          (typeof window !== "undefined" ? window.innerWidth : 0));
  if (stageWidth <= 0) {
    return fallbackRatio;
  }

  const controlPanelWidthRatio =
    resolveRootCssLengthPx(
      "--control-panel-pane-width",
      CONTROL_PANEL_PANE_WIDTH_FALLBACK_PX,
    ) / stageWidth;
  const controlPanelMinRatio = clamp(
    resolveRootCssLengthPx(
      "--control-panel-pane-min-width",
      CONTROL_PANEL_PANE_MIN_WIDTH_FALLBACK_PX,
    ) / stageWidth,
    0,
    1,
  );
  const sessionMinRatio = DEFAULT_SPLIT_MIN_RATIO;
  const maxRatio = 1 - sessionMinRatio;

  if (controlPanelMinRatio <= maxRatio) {
    return clamp(controlPanelWidthRatio, controlPanelMinRatio, maxRatio);
  }

  return clamp(
    controlPanelMinRatio /
      Math.max(controlPanelMinRatio + sessionMinRatio, Number.EPSILON),
    0,
    1,
  );
}

function resolveRootCssLengthPx(
  cssVariableName: string,
  fallbackPx: number,
): number {
  if (typeof window === "undefined" || typeof document === "undefined") {
    return fallbackPx;
  }

  const rootStyle = window.getComputedStyle(document.documentElement);
  const rawValue = rootStyle.getPropertyValue(cssVariableName).trim();
  if (!rawValue) {
    return fallbackPx;
  }

  const rootFontSizePx = Number.parseFloat(rootStyle.fontSize);
  const resolvedValue = rawValue.replace(
    /var\((--[\w-]+)\)/g,
    (_, variableName: string) =>
      rootStyle.getPropertyValue(variableName).trim() || "0",
  );
  const convertLengthToPx = (value: string): number | null => {
    const numericValue = Number.parseFloat(value);
    if (!Number.isFinite(numericValue)) {
      return null;
    }

    if (value.endsWith("rem")) {
      return (
        numericValue * (Number.isFinite(rootFontSizePx) ? rootFontSizePx : 16)
      );
    }

    if (value.endsWith("px") || /^-?\d*\.?\d+$/.test(value)) {
      return numericValue;
    }

    return null;
  };
  const directLengthPx = convertLengthToPx(resolvedValue);
  if (directLengthPx !== null) {
    return directLengthPx;
  }

  const calcMultiplicationMatch = resolvedValue.match(
    /^calc\(\s*([^)]+?)\s*\*\s*([^)]+?)\s*\)$/i,
  );
  if (calcMultiplicationMatch) {
    const left = convertLengthToPx(calcMultiplicationMatch[1].trim());
    const right = Number.parseFloat(calcMultiplicationMatch[2].trim());
    if (left !== null && Number.isFinite(right)) {
      return left * right;
    }

    const rightLengthPx = convertLengthToPx(calcMultiplicationMatch[2].trim());
    const leftScalar = Number.parseFloat(calcMultiplicationMatch[1].trim());
    if (rightLengthPx !== null && Number.isFinite(leftScalar)) {
      return leftScalar * rightLengthPx;
    }
  }

  return fallbackPx;
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

function OrchestratorRuntimeActionButton({
  action,
  orchestratorId,
  isPending,
  disabled,
  onClick,
}: {
  action: "pause" | "resume" | "stop";
  orchestratorId: string;
  isPending: boolean;
  disabled: boolean;
  onClick: () => void;
}) {
  const label =
    action === "pause"
      ? `Pause orchestration ${orchestratorId}`
      : action === "resume"
        ? `Resume orchestration ${orchestratorId}`
        : `Stop orchestration ${orchestratorId}`;
  const title = isPending
    ? action === "pause"
      ? "Pausing orchestration"
      : action === "resume"
        ? "Resuming orchestration"
        : "Stopping orchestration"
    : action === "pause"
      ? "Pause orchestration"
      : action === "resume"
        ? "Resume orchestration"
        : "Stop orchestration";

  return (
    <RuntimeActionButton
      action={action}
      ariaLabel={label}
      title={title}
      classNamePrefix="session-orchestrator-group-action"
      isPending={isPending}
      disabled={disabled}
      onClick={onClick}
    />
  );
}
function SessionFindBar({
  inputRef,
  query,
  activeIndex,
  matches,
  onChange,
  onNext,
  onPrevious,
  onClose,
}: {
  inputRef: RefObject<HTMLInputElement>;
  query: string;
  activeIndex: number;
  matches: SessionSearchMatch[];
  onChange: (nextValue: string) => void;
  onNext: () => void;
  onPrevious: () => void;
  onClose: () => void;
}) {
  const hasQuery = query.trim().length > 0;
  const hasMatches = matches.length > 0;
  const currentMatch =
    hasMatches && activeIndex >= 0 ? (matches[activeIndex] ?? null) : null;
  const countLabel = !hasQuery
    ? "Type to search"
    : hasMatches
      ? `${activeIndex + 1} of ${matches.length}`
      : "No matches";

  return (
    <div
      className="session-find-bar"
      role="search"
      aria-label="Find in session"
    >
      <input
        ref={inputRef}
        className="session-find-input"
        type="search"
        value={query}
        placeholder="Find in session"
        spellCheck={false}
        onChange={(event) => onChange(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            if (event.shiftKey) {
              onPrevious();
            } else {
              onNext();
            }
            return;
          }

          if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
        }}
      />
      <span
        className="session-find-count"
        aria-live="polite"
        title={currentMatch?.snippet ?? undefined}
      >
        {countLabel}
      </span>
      <button
        className="session-find-button"
        type="button"
        onClick={onPrevious}
        disabled={!hasMatches}
      >
        Prev
      </button>
      <button
        className="session-find-button"
        type="button"
        onClick={onNext}
        disabled={!hasMatches}
      >
        Next
      </button>
      <button
        className="session-find-button session-find-close"
        type="button"
        onClick={onClose}
      >
        Close
      </button>
    </div>
  );
}

function resolveWorkspaceTabProjectId(
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
