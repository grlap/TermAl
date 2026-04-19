// Recursive workspace tree renderer: walks the binary tree of panes
// and splits, rendering each leaf as a `<SessionPaneView>` and each
// internal split as two child `<WorkspaceNodeView>` subtrees with a
// resize handle between them.
//
// What this file owns:
//   - `WorkspaceNodeView` — the recursive tree-walking component
//     and its large props surface (~90 props threaded through to
//     leaf panes). Builds the branch class names that key off
//     control-panel vs standalone-control-surface pane widths so
//     the CSS-driven minimum widths line up with `getWorkspaceSplit
//     ResizeBounds` logic, and renders the resize handle with the
//     right `onResizeStart`/`onSplitPane` wiring.
//
// What this file does NOT own:
//   - Leaf-pane rendering — `SessionPaneView` lives in
//     `./SessionPaneView.tsx` and is imported here.
//   - The workspace tree mutations (split, close, move tab,
//     reconcile) — those live in `./workspace.ts`.
//   - The width-math / resize-bounds helpers — `getWorkspaceSplit
//     ResizeBounds`, `workspaceNodeContainsControlPanel`, and
//     `workspaceNodeUsesStandaloneControlSurfaceMinWidth` live in
//     `./workspace-queries.ts`.
//
// Split out of `ui/src/App.tsx`. Same signature and behaviour as
// the inline definition it replaced; imports copied over verbatim
// so the move stays a pure relocation. Unused imports will be
// pruned in a follow-up.

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
  type MouseEvent as ReactMouseEvent,
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
  deleteProject,
  deleteWorkspaceLayout,
  fetchAgentCommands,
  fetchFile,
  fetchGitDiff,
  fetchGitStatus,
  fetchSession,
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
  type CreateSessionResponse,
  type GitDiffRequestPayload,
  type GitDiffSection,
  type OpenPathOptions,
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
  DEFAULT_CLAUDE_APPROVAL_MODE,
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
import { SessionFindBar } from "./SessionFindBar";
import {
  hydrateControlPanelLayout,
  resolveStandaloneControlPanelDockWidthRatio,
} from "./control-panel-layout";
import {
  getWorkspaceSplitResizeBounds,
  resolveControlSurfaceSectionIdForWorkspaceTab,
  resolveWorkspaceTabProjectId,
  workspaceContainsOnlyControlPanel,
  workspaceNodeContainsControlPanel,
  workspaceNodeUsesStandaloneControlSurfaceMinWidth,
} from "./workspace-queries";
import {
  buildControlSurfaceSessionListEntries,
  buildControlSurfaceSessionListState,
  createControlPanelSectionLauncherTab,
  formatSessionOrchestratorGroupName,
  mergeOrchestratorDeltaSessions,
  resolveWorkspaceScopedProjectId,
  resolveWorkspaceScopedSessionId,
} from "./control-surface-state";
import {
  collectGitDiffPreviewRefreshes,
  collectRestoredGitDiffDocumentContentRefreshes,
} from "./git-diff-refresh";
import {
  isSourceFileMissingError,
  sourceFileStateFromResponse,
} from "./source-file-state";
import {
  BACKEND_UNAVAILABLE_ISSUE_DETAIL,
  describeBackendConnectionIssueDetail,
  type BackendConnectionState,
} from "./backend-connection";
import {
  resolveSettledScrollMinimumAttempts,
  syncMessageStackScrollPosition,
} from "./scroll-position";
import {
  resolveAdoptedStateSlices,
  resolveRecoveredWorkspaceLayoutRequestError,
} from "./state-adoption";
import { createInitialWorkspaceBootstrap } from "./initial-workspace-bootstrap";
import { appTestHooks, setAppTestHooksForTests } from "./app-test-hooks";
import { ProjectListSection } from "./ProjectListSection";
import { ALL_PROJECTS_FILTER_ID } from "./project-filters";
import { EmptyState } from "./EmptyState";
import { SessionPaneView } from "./SessionPaneView";

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
import type { RuntimeAction } from "./runtime-action-button";
import { OrchestratorRuntimeActionButton } from "./OrchestratorRuntimeActionButton";
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
  TerminalPanel,
  pruneTerminalPanelHistory,
} from "./panels/TerminalPanel";
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
  stripDiffPreviewDocumentContentFromWorkspaceState,
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
  MARKDOWN_STYLES,
  MARKDOWN_THEMES,
  MAX_DENSITY_PERCENT,
  MAX_EDITOR_FONT_SIZE_PX,
  MAX_FONT_SIZE_PX,
  MIN_DENSITY_PERCENT,
  MIN_EDITOR_FONT_SIZE_PX,
  MIN_FONT_SIZE_PX,
  STYLES,
  THEMES,
  applyDensityPreference,
  applyDiagramLookPreference,
  applyDiagramPalettePreference,
  applyDiagramThemeOverridePreference,
  applyFontSizePreference,
  applyMarkdownStylePreference,
  applyMarkdownThemePreference,
  applyStylePreference,
  applyThemePreference,
  clampDensityPreference,
  clampEditorFontSizePreference,
  clampFontSizePreference,
  persistDensityPreference,
  persistDiagramLookPreference,
  persistDiagramPalettePreference,
  persistDiagramThemeOverridePreference,
  persistEditorFontSizePreference,
  persistFontSizePreference,
  persistMarkdownStylePreference,
  persistMarkdownThemePreference,
  persistStylePreference,
  persistThemePreference,
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
import { startActivePromptPoll } from "./active-prompt-poll";
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
} from "./workspace-file-events";


export function WorkspaceNodeView({
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
  forceSessionScrollToBottomRef,
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
  onOpenTerminalTab,
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
  sessionModelOptionErrors: Record<string, string | undefined>;
  agentCommandsBySessionId: SessionAgentCommandMap;
  refreshingAgentCommandSessionIds: SessionFlagMap;
  agentCommandErrors: Record<string, string | undefined>;
  sessionSettingNotices: Record<string, string | undefined>;
  paneShouldStickToBottomRef: React.MutableRefObject<
    Record<string, boolean | undefined>
  >;
  paneScrollPositionsRef: React.MutableRefObject<
    Record<string, Record<string, { top: number; shouldStick: boolean }>>
  >;
  paneContentSignaturesRef: React.MutableRefObject<
    Record<string, Record<string, string>>
  >;
  forceSessionScrollToBottomRef: React.MutableRefObject<
    Record<string, true | undefined>
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
    options?: OpenPathOptions,
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
  onOpenTerminalTab: (
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
        forceSessionScrollToBottomRef={forceSessionScrollToBottomRef}
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
        onOpenTerminalTab={onOpenTerminalTab}
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
          forceSessionScrollToBottomRef={forceSessionScrollToBottomRef}
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
          onOpenTerminalTab={onOpenTerminalTab}
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
          forceSessionScrollToBottomRef={forceSessionScrollToBottomRef}
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
          onOpenTerminalTab={onOpenTerminalTab}
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
