// Pane view for a single leaf in the workspace binary tree.
//
// What this file owns:
//   - `SessionPaneView` — the large component that renders a single
//     workspace pane: its tab bar (drag/drop, context menu, close),
//     its active-tab body (session transcript, diff preview, source
//     editor, git status, filesystem, orchestrator canvas,
//     terminal, etc.), its composer/footer wiring, and the
//     find-in-session toolbar UI.
//   - All of the component-local useState / useMemo / useEffect /
//     useRef / useLayoutEffect hooks that drive pane-level UI
//     orchestration (session search index + active match tracking,
//     composer paste handling, drag-and-drop from the tab bar, etc.).
//
// What this file does NOT own:
//   - Workspace tree structure or recursion — that lives in
//     `WorkspaceNodeView` (still in `App.tsx` for now). Splitting
//     this pane view lets `WorkspaceNodeView` move next without a
//     circular import.
//   - Any of the extracted helpers it composes (workspace queries,
//     scroll/follow state, source-file loading, state adoption,
//     session find, etc.).
//   - The renderers for the panels themselves — those live under
//     `./panels/`.
//
// Split out of `ui/src/App.tsx`. Same public signature as the inline
// definition it replaced; later pure code moves extracted active-context,
// message projection, scroll/follow state, source-file state, and render
// callback helpers.

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent as ReactClipboardEvent,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import {
  resolveControlPanelWorkspaceRoot,
} from "./session-model-utils";
import {
  ThemedCombobox,
} from "./preferences-panels";
import { SessionFindBar } from "./SessionFindBar";
import { useStableEvent } from "./panels/use-stable-event";
import {
  resolveWorkspaceScopedSessionId,
} from "./control-surface-state";
import { sourceFileStateFromResponse } from "./source-file-state";
import { normalizeDisplayPath } from "./path-display";
import {
  resolvePaneScrollCommand,
} from "./pane-keyboard";
import {
  AgentSessionPanel,
  AgentSessionPanelFooter,
} from "./panels/AgentSessionPanel";
import { useSessionRecordSnapshot } from "./session-store";
import { DiffPanel } from "./panels/DiffPanel";
import { FileSystemPanel } from "./panels/FileSystemPanel";
import { GitStatusPanel } from "./panels/GitStatusPanel";
import { InstructionDebuggerPanel } from "./panels/InstructionDebuggerPanel";
import { PaneTabs } from "./panels/PaneTabs";
import { OrchestratorTemplatesPanel } from "./panels/OrchestratorTemplatesPanel";
import { SessionCanvasPanel } from "./panels/SessionCanvasPanel";
import {
  TerminalPanel,
} from "./panels/TerminalPanel";
import { SourcePanel } from "./panels/SourcePanel";
import {
  buildSessionSearchIndex,
  buildSessionSearchMatchesFromIndex,
} from "./session-find";
import type {
  Session,
} from "./types";
import {
  type SessionPaneViewMode,
  type TabDropPlacement,
  type WorkspacePane,
  type WorkspaceSessionTab,
} from "./workspace";
import {
  dataTransferHasSessionDragType,
} from "./session-drag";
import {
  TAB_DRAG_MIME_TYPE,
  readWorkspaceTabDragData,
  type WorkspaceTabDrag,
} from "./tab-drag";
import {
  buildSessionConversationSignature,
  collectCandidateSourcePaths,
  collectClipboardImageFiles,
  createDraftAttachmentsFromFiles,
  dropLabelForPlacement,
  formatByteSize,
  getErrorMessage,
  isMonacoEditorEventTarget,
  isPointerWithinPaneTopArea,
  labelForPaneViewMode,
  primaryModifierLabel,
  resolvePaneDropPlacementFromPointer,
  resolveLiveWaitingIndicatorPrompt,
  type DraftImageAttachment,
} from "./app-utils";
import {
  buildConnectionRetryDisplayStateByMessageId,
} from "./connection-retry";
import { useStableMapBySignature } from "./use-stable-map-by-signature";
import {
  streamingAssistantTextMessageIdForSession,
  useSessionRenderCallbacks,
} from "./SessionPaneView.render-callbacks";
import { useSessionPaneActiveContext } from "./SessionPaneView.active-context";
import {
  commandMessagesForPaneViewMode,
  diffMessagesForPaneViewMode,
  latestAssistantMessageIdForSession,
  paneViewModeDefaultsToBottomScroll,
  resolveSessionPaneVisibleMessageState,
} from "./SessionPaneView.messages";
import {
  cancelDelegationCommand,
  createComposerDelegationRequest,
  getDelegationResultCommand,
  getDelegationStatusCommand,
  resolveComposerDelegationAvailability,
  spawnDelegationCommand,
  type CreateComposerDelegationOptions,
} from "./delegation-commands";
import {
  delegationWaitIndicatorPrompt,
  hasAgentOutputAfterLatestUserPrompt,
  hasTurnFinalizingOutputAfterLatestUserPrompt,
} from "./SessionPaneView.waiting-indicator";
import { resolveSessionPaneScrollStateKey } from "./SessionPaneView.scroll-key";
import { useSessionPaneScrollState } from "./SessionPaneView.scroll";
import { useSessionPaneSourceFileState } from "./SessionPaneView.source-file";
import type { SessionPaneViewProps } from "./SessionPaneView.types";

export function SessionPaneView({
  pane,
  codexState,
  projectLookup,
  remoteLookup,
  delegationWaits,
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
  paneMessageContentSignaturesRef,
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
  onCreateConversationMarker,
  onDeleteConversationMarker,
  onRollbackCodexThread,
  onUnarchiveCodexThread,
  onOrchestratorStateUpdated,
  renderControlPanel,
  renderControlPanelPaneBarStatus,
  renderControlPanelPaneBarActions,
  workspaceFilesChangedEvent,
  backendConnectionState,
}: SessionPaneViewProps) {
  const firstAvailableSessionTabId =
    pane.tabs.find(
      (tab): tab is WorkspaceSessionTab =>
        tab.kind === "session" && sessionLookup.has(tab.sessionId),
    )?.sessionId ?? null;
  const firstSessionTabId =
    pane.tabs.find(
      (tab): tab is WorkspaceSessionTab => tab.kind === "session",
    )?.sessionId ?? null;
  const activeSessionSnapshotId =
    (pane.activeSessionId && sessionLookup.has(pane.activeSessionId)
      ? pane.activeSessionId
      : null) ??
    firstAvailableSessionTabId ??
    pane.activeSessionId ??
    firstSessionTabId;
  // The session store can receive an eager active-session update before the
  // broader `sessions` prop is reconciled. Use that fresher record for active
  // pane derivation while keeping the prop-backed lookup for unrelated sessions.
  const storeActiveSession = useSessionRecordSnapshot(activeSessionSnapshotId);
  const sessionLookupActiveSession = activeSessionSnapshotId
    ? (sessionLookup.get(activeSessionSnapshotId) ?? null)
    : null;
  // The eager store path is expected to be at least as fresh as the prop-backed
  // lookup for this active session; without a store record, the prop lookup stays
  // authoritative.
  // During this one-render divergence, derive signatures from the store-backed
  // session but defer layout scroll effects until the parent session list catches up.
  const deferStoreBackedScrollEffects =
    !!storeActiveSession && sessionLookupActiveSession !== storeActiveSession;
  const activeContextSessionLookup = useMemo(() => {
    if (!storeActiveSession) {
      return sessionLookup;
    }

    if (sessionLookup.get(storeActiveSession.id) === storeActiveSession) {
      return sessionLookup;
    }

    const nextSessionLookup = new Map(sessionLookup);
    nextSessionLookup.set(storeActiveSession.id, storeActiveSession);
    return nextSessionLookup;
  }, [sessionLookup, storeActiveSession]);
  const {
    activeTab,
    activeControlPanelTab,
    activeOrchestratorListTab,
    activeSessionListTab,
    activeProjectListTab,
    activeCanvasTab,
    activeOrchestratorCanvasTab,
    activeControlSurfaceTab,
    activeSourceTab,
    activeFilesystemTab,
    activeGitStatusTab,
    activeTerminalTab,
    activeInstructionDebuggerTab,
    activeDiffPreviewTab,
    activeSourceOriginSessionId,
    activeSourceOriginProjectId,
    activeFilesystemOriginSessionId,
    activeFilesystemOriginProjectId,
    activeGitStatusOriginSessionId,
    activeGitStatusOriginProjectId,
    activeTerminalOriginSessionId,
    activeTerminalOriginProjectId,
    activeInstructionDebuggerOriginSessionId,
    activeInstructionDebuggerOriginProjectId,
    activeInstructionDebuggerSession,
    activeDiffOriginSessionId,
    activeDiffOriginProjectId,
    activeDiffWorkspaceRoot,
    activeSourceWorkspaceRoot,
    isSessionTabActive,
    sessionTabs,
    activeSession,
    enableLocalDelegationActions,
    allKnownSessions,
    workspaceProjectOptions,
    sessions,
    activeFilesystemScopeProjectId,
    activeGitScopeProjectId,
    activeTerminalScopeProjectId,
    activeFilesystemScopedSessionId,
    activeGitScopedSessionId,
    activeTerminalScopedSessionId,
    activeFilesystemScopedRootPath,
    activeGitScopedWorkdir,
    activeTerminalScopedWorkdir,
    shouldRenderFilesystemProjectScope,
    shouldRenderGitProjectScope,
    shouldRenderTerminalProjectScope,
  } = useSessionPaneActiveContext({
    pane,
    projectLookup,
    sessionLookup: activeContextSessionLookup,
  });
  const paneRootRef = useRef<HTMLElement | null>(null);
  const paneTopRef = useRef<HTMLDivElement | null>(null);
  const [activeDropPlacement, setActiveDropPlacement] = useState<Exclude<
    TabDropPlacement,
    "tabs"
  > | null>(null);
  const [pointerDraggedTab, setPointerDraggedTab] =
    useState<WorkspaceTabDrag | null>(null);
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
  const {
    fileState,
    handleSourceEditorDirtyChange,
    handleSourceFileAdopt,
    handleSourceFileFetchLatest,
    handleSourceFileReload,
    handleSourceFileSave,
    tabDecorations,
  } = useSessionPaneSourceFileState({
    activeSourceOriginProjectId,
    activeSourceOriginSessionId,
    activeSourceTab,
    activeSourceWorkspaceRoot,
    onPaneSourcePathChange,
    paneId: pane.id,
    paneSourcePath: pane.sourcePath,
    paneViewMode: pane.viewMode,
    sourceCandidatePaths,
    workspaceFilesChangedEvent,
  });
  const commandMessages = useMemo(
    () => commandMessagesForPaneViewMode(pane.viewMode, activeSession),
    [activeSession, pane.viewMode],
  );
  const diffMessages = useMemo(
    () => diffMessagesForPaneViewMode(pane.viewMode, activeSession),
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
  const activeDelegationWaits = useMemo(
    () =>
      activeSession
        ? delegationWaits.filter(
            (wait) => wait.parentSessionId === activeSession.id,
          )
        : [],
    [activeSession, delegationWaits],
  );
  const showDelegationWaitIndicator =
    isSessionTabActive &&
    pane.viewMode === "session" &&
    Boolean(activeSession) &&
    !isSessionBusy &&
    !isSending &&
    activeDelegationWaits.length > 0;
  const showLiveTurnWaitingIndicator =
    isSessionTabActive &&
    pane.viewMode === "session" &&
    Boolean(activeSession) &&
    ((activeSession?.status === "active" &&
      !hasTurnFinalizingOutputAfterLatestUserPrompt(
        activeSession?.messages ?? [],
      )) ||
      (!isSessionBusy &&
        isSending &&
        !hasAgentOutputAfterLatestUserPrompt(activeSession?.messages ?? [])));
  const showWaitingIndicator =
    showLiveTurnWaitingIndicator || showDelegationWaitIndicator;
  const activeSessionMessages = activeSession?.messages;
  const activeSessionStatus = activeSession?.status;
  // Delegated children are transcript/control surfaces owned by the parent
  // delegation flow: keep transcript tools reachable, but do not allow prompts
  // or new delegations to be injected from the child pane.
  const isDelegatedChildSession = Boolean(activeSession?.parentDelegationId);
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
    if (showDelegationWaitIndicator) {
      return delegationWaitIndicatorPrompt(activeDelegationWaits);
    }
    if (
      !showLiveTurnWaitingIndicator ||
      !activeSession ||
      (!isSessionBusy && isSending)
    ) {
      return null;
    }

    return resolveLiveWaitingIndicatorPrompt(
      activeSessionMessages && activeSessionStatus
        ? { messages: activeSessionMessages, status: activeSessionStatus }
        : null,
    );
  }, [
    activeDelegationWaits,
    activeSessionMessages,
    activeSessionStatus,
    isSending,
    isSessionBusy,
    showDelegationWaitIndicator,
    showLiveTurnWaitingIndicator,
  ]);
  const waitingIndicatorKind = showDelegationWaitIndicator
    ? "delegationWait"
    : isSending && !isSessionBusy
      ? "send"
    : "liveTurn";
  const composerInputDisabled = !activeSession || isStopping;
  const composerSendDisabled = !activeSession || isSending || isStopping;
  const scrollStateKey = resolveSessionPaneScrollStateKey(
    pane.id,
    pane.viewMode,
    activeSession?.id,
    activeTab,
  );
  const defaultScrollToBottom = paneViewModeDefaultsToBottomScroll(pane.viewMode);
  const {
    visibleMessages,
    visibleContentSignature,
    visibleMessageContentSignature,
    visibleLastMessageAuthor,
  } = useMemo(
    () =>
      resolveSessionPaneVisibleMessageState({
        viewMode: pane.viewMode,
        session: activeSession,
        commandMessages,
        diffMessages,
        sessionConversationSignature,
      }),
    [
      activeSession,
      commandMessages,
      diffMessages,
      pane.viewMode,
      sessionConversationSignature,
    ],
  );
  // Newest assistant message id drives retry-notice liveness. Streaming render
  // preference is narrower: only the active turn's last transcript item can be
  // streaming text, so a previous completed table does not switch render modes
  // while the next prompt is waiting for its first assistant chunk.
  const latestAssistantMessageId = useMemo(
    () => latestAssistantMessageIdForSession(activeSession),
    [activeSession],
  );
  const streamingAssistantTextMessageId = useMemo(
    () => streamingAssistantTextMessageIdForSession(activeSession),
    [activeSession],
  );
  const nextConnectionRetryDisplayStateByMessageId = useMemo(
    () => buildConnectionRetryDisplayStateByMessageId(activeSession),
    [activeSession?.messages, activeSession?.status],
  );
  const connectionRetryDisplayStateByMessageId = useStableMapBySignature(
    nextConnectionRetryDisplayStateByMessageId,
  );
  const getConnectionRetryDisplayState = useCallback(
    (messageId: string) => connectionRetryDisplayStateByMessageId.get(messageId),
    [connectionRetryDisplayStateByMessageId],
  );
  const paneScrollPositions =
    paneScrollPositionsRef.current[pane.id] ??
    (paneScrollPositionsRef.current[pane.id] = {});
  const paneContentSignatures =
    paneContentSignaturesRef.current[pane.id] ??
    (paneContentSignaturesRef.current[pane.id] = {});
  const paneMessageContentSignatures =
    paneMessageContentSignaturesRef.current[pane.id] ??
    (paneMessageContentSignaturesRef.current[pane.id] = {});
  const {
    handleConversationSearchItemMount,
    handleMessageStackScroll,
    handleMessageStackTouchStart,
    handleMessageStackUserScrollIntent,
    liveTailPinned,
    messageStackRef,
    newResponseIndicatorLabel,
    scrollMessageStackByPage,
    scrollMessageStackToBoundary,
    scrollSessionMessageStackByPageJump,
    showNewResponseIndicator,
  } = useSessionPaneScrollState({
    activeSession,
    activeSessionSearchMatch,
    defaultScrollToBottom,
    deferContentScrollEffects: deferStoreBackedScrollEffects,
    forceSessionScrollToBottomRef,
    hasSessionFindQuery,
    isActive,
    isSending,
    isSessionTabActive,
    onScrollToBottomRequestHandled,
    paneContentSignatures,
    paneId: pane.id,
    paneMessageContentSignatures,
    paneRootRef,
    paneScrollPositions,
    paneShouldStickToBottomRef,
    paneViewMode: pane.viewMode,
    pendingScrollToBottomRequest,
    scrollStateKey,
    sessions,
    showWaitingIndicator,
    visibleContentSignature,
    visibleLastMessageAuthor,
    visibleMessageContentSignature,
  });

  const showDelegatedChildFooter =
    isDelegatedChildSession &&
    activeTab?.kind === "session" &&
    pane.viewMode === "session" &&
    Boolean(
      activeSession &&
        (isSessionBusy ||
          isStopping ||
          showNewResponseIndicator),
    );

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

  const handleScrollToLatestFromFooter = useStableEvent(() => {
    scrollMessageStackToBoundary("bottom");
  });
  const handleDraftCommitFromFooter = useStableEvent(onDraftCommit);
  const handleDraftAttachmentRemoveFromFooter = useStableEvent(
    onDraftAttachmentRemove,
  );
  const handleRefreshSessionModelOptionsFromFooter = useStableEvent(
    onRefreshSessionModelOptions,
  );
  const handleRefreshAgentCommandsFromFooter = useStableEvent(
    onRefreshAgentCommands,
  );
  const handleSendFromFooter = useStableEvent(onSend);
  const handleSpawnDelegationFromFooter = useStableEvent(
    async (
      sessionId: string,
      prompt: string,
      options?: CreateComposerDelegationOptions,
    ) => {
      const parentSession = activeContextSessionLookup.get(sessionId);
      if (!parentSession) {
        onComposerError("Session is no longer available.");
        return false;
      }
      const parentProject =
        parentSession.projectId != null
          ? (projectLookup.get(parentSession.projectId) ?? null)
          : null;
      const availability = resolveComposerDelegationAvailability(
        parentSession,
        parentProject,
      );
      if (availability.outcome === "error") {
        onComposerError(availability.message);
        return false;
      }

      const result = await spawnDelegationCommand(
        sessionId,
        createComposerDelegationRequest(parentSession, prompt, options),
      );
      if (result.outcome === "error") {
        // Command wrappers already sanitize validation and transport failures;
        // the composer intentionally shows one retry/fix prompt channel for now.
        onComposerError(result.error.message);
        return false;
      }

      onComposerError(null);
      return true;
    },
  );
  const handleSessionSettingsChangeFromFooter = useStableEvent(
    onSessionSettingsChange,
  );
  const handleStopSessionFromFooter = useStableEvent(onStopSession);
  const handleComposerPasteFromFooter = useStableEvent(handleComposerPaste);

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

  function resolvePaneTabCycleDirection(event: {
    altKey: boolean;
    ctrlKey: boolean;
    key: string;
    metaKey: boolean;
    shiftKey: boolean;
  }): -1 | 1 | null {
    if (
      !event.altKey ||
      event.ctrlKey ||
      event.metaKey ||
      event.shiftKey ||
      (event.key !== "PageUp" && event.key !== "PageDown")
    ) {
      return null;
    }
    return event.key === "PageUp" ? -1 : 1;
  }

  function handlePaneKeyDown(event: ReactKeyboardEvent<HTMLElement>) {
    if (event.defaultPrevented) {
      return;
    }
    if (isMonacoEditorEventTarget(event.target, paneRootRef.current)) {
      return;
    }

    const tabCycleDirection = isActive
      ? resolvePaneTabCycleDirection({
          altKey: event.altKey,
          ctrlKey: event.ctrlKey,
          key: event.key,
          metaKey: event.metaKey,
          shiftKey: event.shiftKey,
        })
      : null;
    if (tabCycleDirection != null) {
      event.preventDefault();
      selectAdjacentPaneTab(tabCycleDirection);
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
    if (!showDropOverlay) {
      setActiveDropPlacement(null);
      setPointerDraggedTab(null);
    }
  }, [showDropOverlay]);

  const delegationActions = useMemo(
    () => ({
      cancel: cancelDelegationCommand,
      getResult: getDelegationResultCommand,
      getStatus: getDelegationStatusCommand,
    }),
    [],
  );

  const {
    renderSessionCommandCard,
    renderSessionDiffCard,
    renderSessionMessageCard,
    renderSessionPromptSettings,
  } = useSessionRenderCallbacks({
    activeSession,
    activeSessionSearchMatchItemKey: activeSessionSearchMatch?.itemKey,
    editorAppearance,
    getConnectionRetryDisplayState,
    isRefreshingModelOptions,
    latestAssistantMessageId,
    streamingAssistantTextMessageId,
    modelOptionsError,
    delegationActions,
    enableLocalDelegationActions,
    onArchiveCodexThread,
    onCompactCodexThread,
    onForkCodexThread,
    onOpenDiffPreviewTab,
    onOpenSourceTab,
    onOpenConversationFromDiff,
    onInsertReviewIntoPrompt,
    onComposerError,
    onRefreshSessionModelOptions,
    onRollbackCodexThread,
    onUnarchiveCodexThread,
    paneId: pane.id,
    sessionFindQuery,
    sessionSettingNotice,
  });

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
        tabIndex={
          activeTab?.kind === "session" && pane.viewMode === "session"
            ? 0
            : undefined
        }
        aria-label={
          activeTab?.kind === "session" && pane.viewMode === "session"
            ? "Session transcript"
            : undefined
        }
        onScroll={handleMessageStackScroll}
        onWheel={handleMessageStackUserScrollIntent}
        onTouchStart={handleMessageStackTouchStart}
        onTouchMove={handleMessageStackUserScrollIntent}
        onKeyDown={handleMessageStackUserScrollIntent}
        // Scrollbar-thumb mousedown is the only path that bypasses
        // wheel/touch/key bindings; without it a scrollbar drag during the
        // pane's `bottom_follow` cooldown would still re-pin the user to the
        // bottom (the onScroll handler treats forward-progress ticks during
        // the cooldown as continuation of the smooth animation).
        onMouseDown={handleMessageStackUserScrollIntent}
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
            sessionLookup={activeContextSessionLookup}
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
            key={activeSourceTab.id ?? activeSourceTab.path}
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
                      activeContextSessionLookup.get(activeSourceOriginSessionId)
                        ?.workdir ?? null,
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
              const response = await handleSourceFileSave(
                path,
                content,
                activeSourceOriginSessionId,
                activeSourceOriginProjectId,
                options,
              );
              return sourceFileStateFromResponse(response);
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
                activeFilesystemScopeProjectId ?? "",
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
                      activeContextSessionLookup,
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
                activeGitScopeProjectId ?? "",
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
                      activeContextSessionLookup,
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
                  activeTerminalScopeProjectId ?? "",
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
                        activeContextSessionLookup,
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
                  ? (activeContextSessionLookup.get(activeDiffOriginSessionId)
                      ?.agent ?? null)
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
            liveTailPinned={liveTailPinned}
            scrollContainerRef={messageStackRef}
            activeSessionId={activeSession?.id ?? null}
            isLoading={isLoading}
            isUpdating={isUpdating}
            showWaitingIndicator={showWaitingIndicator}
            waitingIndicatorKind={waitingIndicatorKind}
            waitingIndicatorPrompt={waitingIndicatorPrompt}
            commandMessages={commandMessages}
            diffMessages={diffMessages}
            onApprovalDecision={onApprovalDecision}
            onUserInputSubmit={onUserInputSubmit}
            onMcpElicitationSubmit={onMcpElicitationSubmit}
            onCodexAppRequestSubmit={onCodexAppRequestSubmit}
            onCancelQueuedPrompt={onCancelQueuedPrompt}
            onCreateConversationMarker={onCreateConversationMarker}
            onDeleteConversationMarker={onDeleteConversationMarker}
            onSessionSettingsChange={onSessionSettingsChange}
            conversationSearchQuery={
              hasSessionFindQuery ? sessionFindQuery : ""
            }
            conversationSearchMatchedItemKeys={sessionSearchMatchedItemKeys}
            conversationSearchActiveItemKey={
              activeSessionSearchMatch?.itemKey ?? null
            }
            onConversationSearchItemMount={handleConversationSearchItemMount}
            renderCommandCard={renderSessionCommandCard}
            renderDiffCard={renderSessionDiffCard}
            renderMessageCard={renderSessionMessageCard}
            renderPromptSettings={renderSessionPromptSettings}
          />
        )}
      </section>
      {showDelegatedChildFooter ? (
        <footer className="composer delegated-child-footer">
          {showNewResponseIndicator ? (
            <button className="new-response-indicator" type="button" onClick={handleScrollToLatestFromFooter}>
              {newResponseIndicatorLabel}
            </button>
          ) : null}
          <div className="delegated-child-footer-status" role="status" aria-live="polite">
            {isSessionBusy ? (
              <span className="activity-spinner delegated-child-footer-spinner" aria-hidden="true" />
            ) : null}
            <span className="delegated-child-footer-copy">
              {isStopping
                ? "Stopping delegated session..."
                : isSessionBusy
                  ? `${activeSession?.agent ?? "Agent"} is running`
                  : "Delegated session"}
            </span>
          </div>
          {activeSession && (isSessionBusy || isStopping) ? (
            <button
              className="ghost-button delegated-child-stop-button"
              type="button"
              onClick={() => handleStopSessionFromFooter(activeSession.id)}
              disabled={isStopping}
            >
              {isStopping ? "Stopping..." : "Stop"}
            </button>
          ) : null}
        </footer>
      ) : activeControlSurfaceTab ||
        activeCanvasTab ||
        activeOrchestratorCanvasTab ||
        activeSourceTab ||
        activeFilesystemTab ||
        activeGitStatusTab ||
        activeTerminalTab ||
        activeInstructionDebuggerTab ||
        activeDiffPreviewTab ||
        isDelegatedChildSession ? null : (
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
          newResponseIndicatorLabel={newResponseIndicatorLabel}
          footerModeLabel={labelForPaneViewMode(pane.lastSessionViewMode)}
          onScrollToLatest={handleScrollToLatestFromFooter}
          onDraftCommit={handleDraftCommitFromFooter}
          onDraftAttachmentRemove={handleDraftAttachmentRemoveFromFooter}
          isRefreshingModelOptions={isRefreshingModelOptions}
          modelOptionsError={modelOptionsError}
          agentCommands={agentCommands}
          hasLoadedAgentCommands={hasLoadedAgentCommands}
          isRefreshingAgentCommands={isRefreshingAgentCommands}
          agentCommandsError={agentCommandsError}
          onRefreshSessionModelOptions={handleRefreshSessionModelOptionsFromFooter}
          onRefreshAgentCommands={handleRefreshAgentCommandsFromFooter}
          onSend={handleSendFromFooter}
          canSpawnDelegation={enableLocalDelegationActions}
          onSpawnDelegation={handleSpawnDelegationFromFooter}
          onSessionSettingsChange={handleSessionSettingsChangeFromFooter}
          onStopSession={handleStopSessionFromFooter}
          onPaste={handleComposerPasteFromFooter}
        />
      )}
    </section>
  );
}
