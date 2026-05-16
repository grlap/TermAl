// Owns: the public prop contract for SessionPaneView.
// Does not own: pane rendering, tab orchestration, or session panel behavior.
// Split from: ui/src/SessionPaneView.tsx.

import type { JSX, MutableRefObject } from "react";
import type {
  AgentCommand,
  ApprovalDecision,
  CodexState,
  CreateConversationMarkerOptions,
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
import type {
  DelegationWaitRecord,
  GitDiffRequestPayload,
  GitDiffSection,
  OpenPathOptions,
  StateResponse,
} from "./api";
import type { BackendConnectionState } from "./backend-connection";
import type { DraftImageAttachment } from "./app-utils";
import type { MonacoAppearance } from "./monaco";
import type { ControlPanelSectionId } from "./panels/ControlPanelSurface";
import type { WorkspaceTabDrag } from "./tab-drag";
import type {
  SessionPaneViewMode,
  TabDropPlacement,
  WorkspacePane,
} from "./workspace";

export type SessionPaneViewProps = {
  pane: WorkspacePane;
  codexState: CodexState;
  projectLookup: Map<string, Project>;
  remoteLookup: Map<string, RemoteConfig>;
  delegationWaits: DelegationWaitRecord[];
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
  paneShouldStickToBottomRef: MutableRefObject<
    Record<string, boolean | undefined>
  >;
  paneScrollPositionsRef: MutableRefObject<
    Record<string, Record<string, { top: number; shouldStick: boolean }>>
  >;
  paneContentSignaturesRef: MutableRefObject<
    Record<string, Record<string, string>>
  >;
  paneMessageContentSignaturesRef: MutableRefObject<
    Record<string, Record<string, string>>
  >;
  forceSessionScrollToBottomRef: MutableRefObject<
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
  onCreateConversationMarker: (
    sessionId: string,
    messageId: string,
    options?: CreateConversationMarkerOptions,
  ) => boolean | void | Promise<boolean | void>;
  onDeleteConversationMarker: (sessionId: string, markerId: string) => void;
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
};
