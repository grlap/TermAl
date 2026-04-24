// Pane view for a single leaf in the workspace binary tree.
//
// What this file owns:
//   - `SessionPaneView` — the large component that renders a single
//     workspace pane: its tab bar (drag/drop, context menu, close),
//     its active-tab body (session transcript, diff preview, source
//     editor, git status, filesystem, orchestrator canvas,
//     terminal, etc.), its composer (prompt input + send /
//     attachments / stop), and the find-in-session toolbar.
//   - All of the component-local useState / useMemo / useEffect /
//     useRef / useLayoutEffect hooks that drive the pane's UI
//     (session search index + active match tracking, composer
//     drag-over state, pending scroll requests, paste handler,
//     drag-and-drop from the tab bar, etc.).
//
// What this file does NOT own:
//   - Workspace tree structure or recursion — that lives in
//     `WorkspaceNodeView` (still in `App.tsx` for now). Splitting
//     this pane view lets `WorkspaceNodeView` move next without a
//     circular import.
//   - Any of the extracted helpers it composes (workspace queries,
//     scroll position, state adoption, session find, etc.).
//   - The renderers for the panels themselves — those live under
//     `./panels/`.
//
// Split out of `ui/src/App.tsx`. Same signature and behaviour as
// the inline definition it replaced; imports are copied over
// verbatim from App.tsx so the move is a pure relocation. Unused
// imports are kept rather than pruned so the diff stays a clean
// code-move; a follow-up can prune them once the component stops
// being a moving target.

import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent as ReactClipboardEvent,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import {
  CommandCard,
  DiffCard,
  MessageCard,
} from "./message-cards";
import {
  fetchFile,
  saveFile,
  type GitDiffRequestPayload,
  type GitDiffSection,
  type OpenPathOptions,
  type StateResponse,
} from "./api";
import {
  resolveControlPanelWorkspaceRoot,
  type ComboboxOption,
} from "./session-model-utils";
import {
  ThemedCombobox,
} from "./preferences-panels";
import { SessionFindBar } from "./SessionFindBar";
import {
  resolveWorkspaceScopedProjectId,
  resolveWorkspaceScopedSessionId,
} from "./control-surface-state";
import {
  isSourceFileMissingError,
  sourceFileStateFromResponse,
} from "./source-file-state";
import {
  type BackendConnectionState,
} from "./backend-connection";
import {
  resolveSettledScrollMinimumAttempts,
  syncMessageStackScrollPosition,
} from "./scroll-position";
import {
  notifyMessageStackScrollWrite,
  type MessageStackScrollWriteKind,
} from "./message-stack-scroll-sync";

import {
  CodexPromptSettingsCard,
  ClaudePromptSettingsCard,
  CursorPromptSettingsCard,
  GeminiPromptSettingsCard,
} from "./prompt-settings-cards";

import { normalizeDisplayPath } from "./path-display";
import {
  resolvePaneScrollCommand,
} from "./pane-keyboard";
import {
  AgentSessionPanel,
  AgentSessionPanelFooter,
} from "./panels/AgentSessionPanel";
import {
  type ControlPanelSectionId,
} from "./panels/ControlPanelSurface";
import { DiffPanel } from "./panels/DiffPanel";
import { FileSystemPanel } from "./panels/FileSystemPanel";
import { GitStatusPanel } from "./panels/GitStatusPanel";
import { InstructionDebuggerPanel } from "./panels/InstructionDebuggerPanel";
import { PaneTabs, type PaneTabDecoration } from "./panels/PaneTabs";
import { OrchestratorTemplatesPanel } from "./panels/OrchestratorTemplatesPanel";
import { SessionCanvasPanel } from "./panels/SessionCanvasPanel";
import {
  TerminalPanel,
} from "./panels/TerminalPanel";
import {
  SourcePanel,
  type SourceFileState,
  type SourceSaveOptions,
} from "./panels/SourcePanel";
import {
  buildSessionSearchIndex,
  buildSessionSearchMatchesFromIndex,
} from "./session-find";
import type {
  ApprovalDecision,
  AgentCommand,
  CommandMessage,
  CodexState,
  DiffMessage,
  JsonValue,
  McpElicitationAction,
  Project,
  RemoteConfig,
  Session,
  SessionSettingsField,
  SessionSettingsValue,
  WorkspaceFilesChangedEvent,
} from "./types";
import {
  type SessionPaneViewMode,
  type TabDropPlacement,
  type WorkspacePane,
} from "./workspace";
import {
  dataTransferHasSessionDragType,
} from "./session-drag";
import type { MonacoAppearance } from "./monaco";
import {
  TAB_DRAG_MIME_TYPE,
  readWorkspaceTabDragData,
  type WorkspaceTabDrag,
} from "./tab-drag";
import {
  canNestedScrollableConsumeWheel,
  clamp,
  buildMessageListSignature,
  buildSessionConversationSignature,
  collectCandidateSourcePaths,
  collectClipboardImageFiles,
  createDraftAttachmentsFromFiles,
  dropLabelForPlacement,
  findLastUserPrompt,
  formatByteSize,
  getErrorMessage,
  isPointerWithinPaneTopArea,
  labelForPaneViewMode,
  normalizeWheelDelta,
  primaryModifierLabel,
  pruneSessionFlags,
  resolvePaneDropPlacementFromPointer,
  type DraftImageAttachment,
} from "./app-utils";
import {
  workspaceFilesChangedEventChangeForPath,
} from "./workspace-file-events";

const SESSION_PAGE_JUMP_VIEWPORT_FACTOR = 0.45;

export function SessionPaneView({
  pane,
  codexState,
  projectLookup,
  remoteLookup,
  sessionLookup,
  isActive,
  isLoading,
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
  const activeTerminalTab = activeTab?.kind === "terminal" ? activeTab : null;
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
  const activeTerminalOriginSessionId =
    activeTerminalTab?.originSessionId ?? null;
  const activeTerminalOriginProjectId =
    activeTerminalTab?.originProjectId ?? null;
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
  const activeTerminalScopeProjectId = activeTerminalTab
    ? resolveWorkspaceScopedProjectId(
        activeTerminalOriginProjectId,
        activeTerminalOriginSessionId,
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
  const activeTerminalScopedSessionId = activeTerminalScopeProjectId
    ? resolveWorkspaceScopedSessionId(
        activeTerminalScopeProjectId,
        activeTerminalOriginSessionId,
        activeSession,
        allKnownSessions,
        sessionLookup,
      )
    : activeTerminalOriginSessionId;
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
  const activeTerminalScopedWorkdir =
    activeTerminalTab?.workdir ??
    (activeTerminalScopeProjectId
      ? resolveControlPanelWorkspaceRoot(
          projectLookup.get(activeTerminalScopeProjectId) ?? null,
          null,
        )
      : null);
  const shouldRenderFilesystemProjectScope =
    !!activeFilesystemScopeProjectId && workspaceProjectOptions.length > 0;
  const shouldRenderGitProjectScope =
    !!activeGitScopeProjectId && workspaceProjectOptions.length > 0;
  const shouldRenderTerminalProjectScope =
    !!activeTerminalScopeProjectId && workspaceProjectOptions.length > 0;
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
  const paneRootRef = useRef<HTMLElement | null>(null);
  const settledScrollToBottomCancelRef = useRef<(() => void) | null>(null);
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
  const [sessionFindFocusRequest, setSessionFindFocusRequest] = useState<{
    selectAll: boolean;
  } | null>(null);
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
            : activeTerminalTab
              ? `${pane.id}:terminal:${activeTerminalTab.id}`
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
  // Newest assistant message id, used by `ConnectionRetryCard` to tell whether
  // a retry notice is still the live one (keep spinning) or historical (the
  // reconnect obviously succeeded because later assistant output exists).
  const latestAssistantMessageId = useMemo(() => {
    const sessionMessages = activeSession?.messages ?? [];
    for (let index = sessionMessages.length - 1; index >= 0; index -= 1) {
      const candidate = sessionMessages[index];
      if (candidate && candidate.author === "assistant") {
        return candidate.id;
      }
    }
    return null;
  }, [activeSession?.messages]);
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
    setSessionFindFocusRequest({
      selectAll,
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

  function scrollToLatestMessage(behavior: ScrollBehavior, force = false) {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    const nextScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    if (force || Math.abs(node.scrollTop - nextScrollTop) > 1) {
      node.scrollTo({
        top: nextScrollTop,
        behavior,
      });
      notifyMessageStackScrollWrite(node);
    }
    setShouldStickToBottom(true);
    paneScrollPositions[scrollStateKey] = {
      top: nextScrollTop,
      shouldStick: true,
    };
    setNewResponseIndicator(scrollStateKey, false);
  }

  function scrollMessageStackByDelta(
    deltaY: number,
    options: {
      scrollKind?: MessageStackScrollWriteKind;
    } = {},
  ) {
    const node = messageStackRef.current;
    if (!node) {
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

    node.scrollTop = nextScrollTop;
    notifyMessageStackScrollWrite(node, {
      scrollKind: options.scrollKind,
    });
    const { shouldStick } = syncMessageStackScrollPosition(
      node,
      scrollStateKey,
      paneScrollPositions,
    );
    setShouldStickToBottom(shouldStick);
    if (shouldStick) {
      setNewResponseIndicator(scrollStateKey, false);
    } else {
      cancelSettledScrollToBottom();
    }
  }

  function isMessageStackNearBottom() {
    const node = messageStackRef.current;
    if (!node) {
      return true;
    }
    return node.scrollHeight - node.scrollTop - node.clientHeight < 72;
  }

  function followLatestMessageForPromptSend() {
    if (isMessageStackNearBottom()) {
      scrollToLatestMessage("smooth");
      return undefined;
    }

    return scheduleSettledScrollToBottom("auto", {
      maxAttempts: 24,
      minAttempts: 4,
    });
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
    notifyMessageStackScrollWrite(node);
  }

  function scrollSessionMessageStackByPageJump(direction: -1 | 1) {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    const distance = Math.max(
      Math.round(node.clientHeight * SESSION_PAGE_JUMP_VIEWPORT_FACTOR),
      1,
    );
    scrollMessageStackByDelta(distance * direction, {
      scrollKind: "page_jump",
    });
  }

  function scrollMessageStackToBoundary(boundary: "top" | "bottom") {
    if (boundary === "bottom") {
      // Expected for long virtualized sessions: jump-to-latest keeps correcting
      // for a few frames while message measurements settle.
      scheduleSettledScrollToBottom("auto", {
        maxAttempts: 60,
        minAttempts: 8,
      });
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

    cancelSettledScrollToBottom();
    node.scrollTo({
      top: 0,
      behavior: "auto",
    });
    notifyMessageStackScrollWrite(node, {
      scrollKind: "seek",
    });
    setShouldStickToBottom(false);
    paneScrollPositions[scrollStateKey] = {
      top: 0,
      shouldStick: false,
    };
  }

  function selectAdjacentPaneTab(direction: -1 | 1) {
    if (pane.tabs.length <= 1) {
      return;
    }

    const activeIndex = pane.tabs.findIndex((tab) => tab.id === activeTab?.id);
    const currentIndex = activeIndex >= 0 ? activeIndex : 0;
    const nextIndex =
      (currentIndex + direction + pane.tabs.length) % pane.tabs.length;
    const nextTab = pane.tabs[nextIndex];
    if (!nextTab) {
      return;
    }
    onSelectTab(pane.id, nextTab.id);
  }

  function isNestedEditablePageKeyTarget(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) {
      return false;
    }

    if (
      target instanceof HTMLTextAreaElement ||
      target instanceof HTMLInputElement ||
      target instanceof HTMLSelectElement
    ) {
      return true;
    }

    return (
      target.isContentEditable ||
      target.contentEditable === "true" ||
      target.getAttribute("contenteditable") === "" ||
      target.getAttribute("contenteditable") === "true"
    );
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
    if (
      pane.viewMode === "session" &&
      command.kind === "page" &&
      (event.key === "PageUp" || event.key === "PageDown")
    ) {
      scrollSessionMessageStackByPageJump(
        command.direction === "up" ? -1 : 1,
      );
      return;
    }
    if (command.kind === "boundary") {
      scrollMessageStackToBoundary(
        command.direction === "up" ? "top" : "bottom",
      );
    } else {
      scrollMessageStackByPage(command.direction === "up" ? -1 : 1);
    }
  }

  // The message-stack wheel handler used to be wired via the React
  // `onWheel` prop on the `<section>` below. React attaches `wheel`
  // listeners as `{ passive: true }` by default (since React 17), so
  // the `event.preventDefault()` call inside this handler silently
  // failed with an `Unable to preventDefault inside passive event
  // listener invocation` warning spammed to the console on every
  // wheel tick. The browser's native scroll then executed alongside
  // this handler's custom `node.scrollTop = nextScrollTop` write —
  // two scrolls per tick, producing the jagged scroll-up experience
  // users reported.
  //
  // We fix this by registering the listener ourselves via
  // `addEventListener(..., { passive: false })`, which lets
  // `preventDefault` take effect and keeps the custom scroll as the
  // single source of truth. A ref indirection keeps the listener
  // registration stable across renders while the closure picks up
  // fresh state every render.
  const handleMessageStackWheelRef = useRef<((event: WheelEvent) => void) | null>(null);
  handleMessageStackWheelRef.current = function handleMessageStackWheel(event: WheelEvent) {
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

    event.preventDefault();
    scrollMessageStackByDelta(deltaY);
  };

  useEffect(() => {
    const node = messageStackRef.current;
    if (!node) {
      return;
    }
    const listener = (event: WheelEvent) => {
      handleMessageStackWheelRef.current?.(event);
    };
    node.addEventListener("wheel", listener, { passive: false });
    return () => {
      node.removeEventListener("wheel", listener);
    };
  }, []);

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

  useLayoutEffect(() => {
    if (!isSessionFindOpen || !sessionFindFocusRequest) {
      return;
    }

    const frameId = window.requestAnimationFrame(() => {
      const input = sessionFindInputRef.current;
      if (!input) {
        return;
      }

      input.focus();
      if (sessionFindFocusRequest.selectAll) {
        input.select();
      }
    });

    return () => {
      window.cancelAnimationFrame(frameId);
    };
  }, [isSessionFindOpen, sessionFindFocusRequest]);

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

    window.addEventListener("keydown", handleWindowKeyDown, true);
    return () => {
      window.removeEventListener("keydown", handleWindowKeyDown, true);
    };
  }, [canFindInSession, isActive]);

  useEffect(() => {
    if (!isActive) {
      return;
    }

    function handleWindowPaneTabCycle(event: KeyboardEvent) {
      if (
        event.defaultPrevented ||
        !event.altKey ||
        event.ctrlKey ||
        event.metaKey ||
        event.shiftKey ||
        (event.key !== "PageUp" && event.key !== "PageDown")
      ) {
        return;
      }

      event.preventDefault();
      selectAdjacentPaneTab(event.key === "PageUp" ? -1 : 1);
    }

    window.addEventListener("keydown", handleWindowPaneTabCycle, true);
    return () => {
      window.removeEventListener("keydown", handleWindowPaneTabCycle, true);
    };
  }, [activeTab?.id, isActive, onSelectTab, pane.id, pane.tabs]);

  const handleNestedTargetPageKeyRef = useRef<((event: KeyboardEvent) => void) | null>(null);
  handleNestedTargetPageKeyRef.current = function handleNestedTargetPageKey(
    event: KeyboardEvent,
  ) {
    if (
      event.defaultPrevented ||
      (event.key !== "PageUp" && event.key !== "PageDown") ||
      !isNestedEditablePageKeyTarget(event.target)
    ) {
      return;
    }
    if (
      !(event.target instanceof Node) ||
      !paneRootRef.current?.contains(event.target)
    ) {
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
      return;
    }

    scrollSessionMessageStackByPageJump(
      command.direction === "up" ? -1 : 1,
    );
  };

  useEffect(() => {
    if (!isActive || pane.viewMode !== "session") {
      return;
    }

    const listener = (event: KeyboardEvent) => {
      handleNestedTargetPageKeyRef.current?.(event);
    };
    window.addEventListener("keydown", listener, true);
    return () => {
      window.removeEventListener("keydown", listener, true);
    };
  }, [isActive, pane.viewMode]);

  function scheduleSettledScrollToBottom(
    behavior: ScrollBehavior,
    options: {
      maxAttempts?: number;
      minAttempts?: number;
      onComplete?: () => void;
    } = {},
  ) {
    cancelSettledScrollToBottom();

    let frameId = 0;
    let cancelled = false;
    let completed = false;
    const maxAttempts = options.maxAttempts ?? 12;
    let remainingAttempts = maxAttempts;
    const minimumAttempts = resolveSettledScrollMinimumAttempts(
      maxAttempts,
      options.minAttempts,
    );
    let attemptCount = 0;
    let previousScrollHeight = -1;
    let stableFrameCount = 0;

    function complete() {
      if (cancelled || completed) {
        return;
      }

      completed = true;
      if (settledScrollToBottomCancelRef.current === cancel) {
        settledScrollToBottomCancelRef.current = null;
      }
      options.onComplete?.();
    }

    const tick = () => {
      frameId = 0;
      attemptCount += 1;
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

      scrollToLatestMessage(behavior, attemptCount <= minimumAttempts);

      const bottomGap = Math.max(
        node.scrollHeight - node.clientHeight - node.scrollTop,
        0,
      );
      const heightStable =
        previousScrollHeight >= 0 &&
        Math.abs(node.scrollHeight - previousScrollHeight) <= 16;
      if (bottomGap <= 4 && heightStable) {
        stableFrameCount += 1;
      } else {
        stableFrameCount = 0;
      }

      previousScrollHeight = node.scrollHeight;
      remainingAttempts -= 1;
      if (
        remainingAttempts > 0 &&
        (attemptCount < minimumAttempts || stableFrameCount < 2)
      ) {
        frameId = window.requestAnimationFrame(tick);
      } else {
        complete();
      }
    };

    const cancel = () => {
      cancelled = true;
      if (frameId !== 0) {
        window.cancelAnimationFrame(frameId);
      }
      if (settledScrollToBottomCancelRef.current === cancel) {
        settledScrollToBottomCancelRef.current = null;
      }
    };

    settledScrollToBottomCancelRef.current = cancel;
    tick();
    return cancel;
  }

  function cancelSettledScrollToBottom() {
    const cancel = settledScrollToBottomCancelRef.current;
    settledScrollToBottomCancelRef.current = null;
    cancel?.();
  }

  function restoreMessageStackScrollTop(targetTop: number) {
    const node = messageStackRef.current;
    if (!node) {
      return false;
    }

    const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
    if (targetTop > maxScrollTop + 1) {
      return false;
    }

    const nextTop = clamp(targetTop, 0, maxScrollTop);
    node.scrollTop = nextTop;
    notifyMessageStackScrollWrite(node);
    paneScrollPositions[scrollStateKey] = {
      top: targetTop,
      shouldStick: false,
    };
    return true;
  }

  useLayoutEffect(() => {
    let restoreCleanup: (() => void) | undefined;
    const node = messageStackRef.current;
    if (!node) {
      return undefined;
    }

    const shouldForceBottomAfterWorkspaceRebuild =
      defaultScrollToBottom &&
      activeSession &&
      forceSessionScrollToBottomRef.current[activeSession.id];
    if (shouldForceBottomAfterWorkspaceRebuild) {
      delete forceSessionScrollToBottomRef.current[activeSession.id];
      setShouldStickToBottom(true);
      paneScrollPositions[scrollStateKey] = {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      };
      restoreCleanup = scheduleSettledScrollToBottom("auto", {
        maxAttempts: 60,
      });
    } else if (paneScrollPositions[scrollStateKey]) {
      const saved = paneScrollPositions[scrollStateKey];
      setShouldStickToBottom(saved.shouldStick);
      if (saved.shouldStick) {
        restoreCleanup = scheduleSettledScrollToBottom("auto", {
          maxAttempts: 60,
        });
      } else if (!restoreMessageStackScrollTop(saved.top)) {
        setShouldStickToBottom(true);
        restoreCleanup = scheduleSettledScrollToBottom("auto", {
          maxAttempts: 60,
        });
      }
    } else if (defaultScrollToBottom) {
      restoreCleanup = scheduleSettledScrollToBottom("auto", {
        maxAttempts: 60,
      });
      setShouldStickToBottom(true);
      paneScrollPositions[scrollStateKey] = {
        top: Number.MAX_SAFE_INTEGER,
        shouldStick: true,
      };
    } else {
      node.scrollTop = 0;
      notifyMessageStackScrollWrite(node);
      setShouldStickToBottom(false);
      paneScrollPositions[scrollStateKey] = {
        top: 0,
        shouldStick: false,
      };
    }

    return () => {
      restoreCleanup?.();
    };
  }, [activeSession?.id, defaultScrollToBottom, scrollStateKey]);

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
    notifyMessageStackScrollWrite(container);

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
  }, [activeSession?.id]);

  useEffect(() => {
    const availableSessionIds = new Set(sessions.map((session) => session.id));
    setVisitedSessionIds((current) =>
      pruneSessionFlags(current, availableSessionIds),
    );
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
      const saved = paneScrollPositions[scrollStateKey];
      if (saved && !saved.shouldStick) {
        if (!restoreMessageStackScrollTop(saved.top)) {
          setShouldStickToBottom(true);
          return scheduleSettledScrollToBottom("auto", { maxAttempts: 60 });
        }
        return;
      }
      if (getShouldStickToBottom() || saved?.shouldStick) {
        return scheduleSettledScrollToBottom("auto", { maxAttempts: 60 });
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

    if (visibleLastMessageAuthor === "you") {
      setNewResponseIndicator(scrollStateKey, false);
      let cleanup: (() => void) | undefined;
      const frameId = window.requestAnimationFrame(() => {
        cleanup = followLatestMessageForPromptSend();
      });
      return () => {
        window.cancelAnimationFrame(frameId);
        cleanup?.();
      };
    }

    setNewResponseIndicator(scrollStateKey, false);
    const frameId = window.requestAnimationFrame(() => {
      scrollToLatestMessage("auto");
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

    let cleanup: (() => void) | undefined;
    const frameId = window.requestAnimationFrame(() => {
      cleanup = followLatestMessageForPromptSend();
    });

    return () => {
      window.cancelAnimationFrame(frameId);
      cleanup?.();
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
      ref={paneRootRef}
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
            {activeControlSurfaceTab ? renderControlPanelPaneBarStatus() : null}
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
                  onOpenTerminalTab(
                    pane.id,
                    activeSession?.workdir ?? null,
                    activeSession?.id ?? null,
                    activeSession?.projectId ?? null,
                  )
                }
              >
                Terminal
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
        className={`message-stack${activeControlSurfaceTab || activeOrchestratorCanvasTab ? " control-panel-stack" : ""}${activeSourceTab || activeDiffPreviewTab ? " editor-panel-stack" : ""}${activeTerminalTab ? " terminal-panel-stack" : ""}`}
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
          } else {
            cancelSettledScrollToBottom();
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
            workspaceRoot={activeSourceWorkspaceRoot}
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
            onOpenSourceLink={(target) =>
              onOpenSourceTab(
                pane.id,
                target.path,
                activeSourceOriginSessionId,
                activeSourceOriginProjectId,
                {
                  line: target.line,
                  column: target.column,
                  openInNewTab: target.openInNewTab,
                },
              )
            }
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
        ) : activeTerminalTab ? (
          <section
            className="control-panel-section-stack terminal-section-stack"
            aria-label="Terminal"
          >
            {shouldRenderTerminalProjectScope
              ? renderWorkspaceTabProjectScope(
                  `workspace-project-scope-${pane.id}-terminal`,
                  activeTerminalScopeProjectId,
                  (nextProjectId) => {
                    const nextProject =
                      projectLookup.get(nextProjectId) ?? null;
                    if (!nextProject) {
                      return;
                    }

                    onOpenTerminalTab(
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
                )
              : null}
            <TerminalPanel
              key={activeTerminalTab.id}
              terminalId={activeTerminalTab.id}
              projectId={
                shouldRenderTerminalProjectScope
                  ? activeTerminalScopeProjectId
                  : activeTerminalOriginProjectId
              }
              sessionId={
                shouldRenderTerminalProjectScope
                  ? activeTerminalScopedSessionId
                  : activeTerminalOriginSessionId
              }
              showPathControls={!shouldRenderTerminalProjectScope}
              workdir={
                shouldRenderTerminalProjectScope
                  ? activeTerminalScopedWorkdir
                  : activeTerminalTab.workdir
              }
              onOpenWorkdir={(path) =>
                onOpenTerminalTab(
                  pane.id,
                  path,
                  shouldRenderTerminalProjectScope
                    ? activeTerminalScopedSessionId
                    : activeTerminalOriginSessionId,
                  shouldRenderTerminalProjectScope
                    ? activeTerminalScopeProjectId
                    : activeTerminalOriginProjectId,
                )
              }
            />
          </section>
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
              documentEnrichmentNote={
                activeDiffPreviewTab.documentEnrichmentNote ?? null
              }
              documentContent={activeDiffPreviewTab.documentContent ?? null}
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
              onOpenPath={(path, options) =>
                onOpenSourceTab(
                  pane.id,
                  path,
                  activeDiffOriginSessionId,
                  activeDiffOriginProjectId,
                  options,
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
              onSaveFile={(path, content, options) =>
                handleSourceFileSave(
                  path,
                  content,
                  activeDiffOriginSessionId,
                  activeDiffOriginProjectId,
                  options,
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
            activeSessionId={activeSession?.id ?? null}
            isLoading={isLoading}
            isUpdating={isUpdating}
            showWaitingIndicator={showWaitingIndicator}
            waitingIndicatorPrompt={waitingIndicatorPrompt}
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
                appearance={editorAppearance}
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
                preferStreamingPlainTextRender={
                  activeSession?.status === "active" &&
                  message.id === latestAssistantMessageId
                }
                isLatestAssistantMessage={
                  message.id === latestAssistantMessageId
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
      activeTerminalTab ||
      activeInstructionDebuggerTab ||
      activeDiffPreviewTab ? null : (
        <AgentSessionPanelFooter
          paneId={pane.id}
          viewMode={pane.viewMode}
          isPaneActive={isActive}
          activeSessionId={activeSession?.id ?? null}
          formatByteSize={formatByteSize}
          isSending={isSending}
          isStopping={isStopping}
          isSessionBusy={isSessionBusy}
          isUpdating={isUpdating}
          showNewResponseIndicator={showNewResponseIndicator}
          footerModeLabel={labelForPaneViewMode(pane.lastSessionViewMode)}
          onScrollToLatest={() => scrollMessageStackToBoundary("bottom")}
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
