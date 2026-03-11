import {
  memo,
  startTransition,
  useEffect,
  useId,
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
} from "react";
import { createPortal } from "react-dom";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  cancelQueuedPrompt,
  createSession,
  fetchFile,
  fetchState,
  killSession,
  sendMessage,
  stopSession,
  submitApproval,
  type StateResponse,
  updateSessionSettings,
} from "./api";
import { highlightCode } from "./highlight";
import { applyDeltaToSessions } from "./live-updates";
import type {
  ApprovalDecision,
  ApprovalMessage,
  ApprovalPolicy,
  AgentType,
  ClaudeApprovalMode,
  CommandMessage,
  CodexRateLimitWindow,
  CodexState,
  DeltaEvent,
  DiffMessage,
  ImageAttachment,
  MarkdownMessage,
  Message,
  PendingPrompt,
  SandboxMode,
  Session,
  TextMessage,
  ThinkingMessage,
} from "./types";
import {
  activatePane,
  closeSessionTab,
  getSplitRatio,
  openSessionInWorkspaceState,
  placeDraggedSession,
  reconcileWorkspaceState,
  setPaneSourcePath,
  setPaneViewMode,
  splitPane,
  updateSplitRatio,
  type PaneViewMode,
  type TabDropPlacement,
  type WorkspaceNode,
  type WorkspacePane,
  type WorkspaceState,
} from "./workspace";
import { reconcileSessions } from "./session-reconcile";
import {
  THEMES,
  applyThemePreference,
  getStoredThemePreference,
  persistThemePreference,
  type ThemeId,
} from "./themes";
import {
  countSessionsByFilter,
  filterSessionsByListFilter,
  type SessionListFilter,
} from "./session-list-filter";

type SessionFlagMap = Record<string, true | undefined>;
type SessionSettingsField = "sandboxMode" | "approvalPolicy" | "claudeApprovalMode";
type SessionSettingsValue = SandboxMode | ApprovalPolicy | ClaudeApprovalMode;
type PreferencesTabId = "themes" | "codex-prompts" | "claude-approvals";
type PromptHistoryState = {
  index: number;
  draft: string;
};
type DraftImageAttachment = ImageAttachment & {
  base64Data: string;
  id: string;
  previewUrl: string;
};
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

const SUPPORTED_PASTED_IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
const MAX_PASTED_IMAGE_BYTES = 5 * 1024 * 1024;
const DEFERRED_RENDER_ROOT_MARGIN_PX = 960;
const HEAVY_CODE_CHARACTER_THRESHOLD = 1400;
const HEAVY_CODE_LINE_THRESHOLD = 28;
const HEAVY_MARKDOWN_CHARACTER_THRESHOLD = 1800;
const HEAVY_MARKDOWN_LINE_THRESHOLD = 24;
const DEFERRED_PREVIEW_LINE_LIMIT = 12;
const DEFERRED_PREVIEW_CHARACTER_LIMIT = 720;
const MAX_DEFERRED_PLACEHOLDER_HEIGHT = 960;
const MAX_CACHED_SESSION_PAGES_PER_PANE = 3;
const CONVERSATION_VIRTUALIZATION_MIN_MESSAGES = 80;
const VIRTUALIZED_MESSAGE_OVERSCAN_PX = 960;
const VIRTUALIZED_MESSAGE_GAP_PX = 12;
const DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT = 720;
const NEW_SESSION_AGENT_OPTIONS = [
  { label: "Claude", value: "Claude" },
  { label: "Codex", value: "Codex" },
] as const;
const SANDBOX_MODE_OPTIONS = [
  { label: "workspace-write", value: "workspace-write" },
  { label: "read-only", value: "read-only" },
  { label: "danger-full-access", value: "danger-full-access" },
] as const;
const APPROVAL_POLICY_OPTIONS = [
  { label: "never", value: "never" },
  { label: "on-request", value: "on-request" },
  { label: "untrusted", value: "untrusted" },
  { label: "on-failure", value: "on-failure" },
] as const;
const CLAUDE_APPROVAL_OPTIONS = [
  { label: "ask", value: "ask" },
  { label: "auto-approve", value: "auto-approve" },
] as const;
const PREFERENCES_TABS: ReadonlyArray<{ id: PreferencesTabId; label: string }> = [
  { id: "themes", label: "Themes" },
  { id: "codex-prompts", label: "Codex prompts" },
  { id: "claude-approvals", label: "Claude approvals" },
];

type ComboboxOption = {
  label: string;
  value: string;
};

export default function App() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [codexState, setCodexState] = useState<CodexState>({});
  const [workspace, setWorkspace] = useState<WorkspaceState>({
    root: null,
    panes: [],
    activePaneId: null,
  });
  const [draftsBySessionId, setDraftsBySessionId] = useState<Record<string, string>>({});
  const [draftAttachmentsBySessionId, setDraftAttachmentsBySessionId] = useState<
    Record<string, DraftImageAttachment[]>
  >({});
  const [newSessionAgent, setNewSessionAgent] = useState<AgentType>("Codex");
  const [isLoading, setIsLoading] = useState(true);
  const [isCreating, setIsCreating] = useState(false);
  const [sendingSessionIds, setSendingSessionIds] = useState<SessionFlagMap>({});
  const [stoppingSessionIds, setStoppingSessionIds] = useState<SessionFlagMap>({});
  const [killingSessionIds, setKillingSessionIds] = useState<SessionFlagMap>({});
  const [killRevealSessionId, setKillRevealSessionId] = useState<string | null>(null);
  const [pendingKillSessionId, setPendingKillSessionId] = useState<string | null>(null);
  const [updatingSessionIds, setUpdatingSessionIds] = useState<SessionFlagMap>({});
  const [requestError, setRequestError] = useState<string | null>(null);
  const [sessionListFilter, setSessionListFilter] = useState<SessionListFilter>("all");
  const [themeId, setThemeId] = useState<ThemeId>(() => getStoredThemePreference());
  const [defaultCodexSandboxMode, setDefaultCodexSandboxMode] =
    useState<SandboxMode>("workspace-write");
  const [defaultCodexApprovalPolicy, setDefaultCodexApprovalPolicy] =
    useState<ApprovalPolicy>("never");
  const [defaultClaudeApprovalMode, setDefaultClaudeApprovalMode] =
    useState<ClaudeApprovalMode>("ask");
  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState<PreferencesTabId>("themes");
  const [pendingScrollToBottomRequest, setPendingScrollToBottomRequest] = useState<{
    sessionId: string;
    token: number;
  } | null>(null);
  const [draggedTab, setDraggedTab] = useState<{
    sourcePaneId: string;
    sessionId: string;
  } | null>(null);
  const resizeStateRef = useRef<{
    splitId: string;
    direction: "row" | "column";
    startRatio: number;
    startX: number;
    startY: number;
    size: number;
  } | null>(null);
  const draftAttachmentsRef = useRef<Record<string, DraftImageAttachment[]>>({});
  const sessionsRef = useRef<Session[]>([]);
  const liveStateEpochRef = useRef(0);
  const stateResyncInFlightRef = useRef(false);
  const stateResyncPendingRef = useRef(false);
  const paneShouldStickToBottomRef = useRef<Record<string, boolean | undefined>>({});
  const paneScrollPositionsRef = useRef<
    Record<string, Record<string, { top: number; shouldStick: boolean }>>
  >({});
  const paneContentSignaturesRef = useRef<Record<string, Record<string, string>>>({});

  const sessionLookup = new Map(sessions.map((session) => [session.id, session]));
  const paneLookup = new Map(workspace.panes.map((pane) => [pane.id, pane]));
  const activePane =
    workspace.panes.find((pane) => pane.id === workspace.activePaneId) ?? workspace.panes[0] ?? null;
  const activeSession = activePane?.activeSessionId
    ? (sessionLookup.get(activePane.activeSessionId) ?? null)
    : null;
  const openSessionIds = useMemo(
    () => new Set(workspace.panes.flatMap((pane) => pane.sessionIds)),
    [workspace.panes],
  );
  const sessionFilterCounts = useMemo(() => countSessionsByFilter(sessions), [sessions]);
  const filteredSessions = useMemo(() => {
    return filterSessionsByListFilter(sessions, sessionListFilter);
  }, [sessionListFilter, sessions]);
  const activeTheme = THEMES.find((theme) => theme.id === themeId) ?? THEMES[0];

  function adoptSessions(
    nextSessions: Session[],
    options?: { openSessionId?: string; paneId?: string | null },
  ) {
    const mergedSessions = reconcileSessions(sessionsRef.current, nextSessions);
    const availableSessionIds = new Set(mergedSessions.map((session) => session.id));

    sessionsRef.current = mergedSessions;
    setSessions(mergedSessions);
    setWorkspace((current) => {
      const reconciled = reconcileWorkspaceState(current, mergedSessions);
      if (!options?.openSessionId) {
        return reconciled;
      }

      return openSessionInWorkspaceState(reconciled, options.openSessionId, options.paneId ?? null);
    });
    setDraftsBySessionId((current) => pruneSessionValues(current, availableSessionIds));
    setDraftAttachmentsBySessionId((current) =>
      pruneSessionAttachmentValues(current, availableSessionIds),
    );
    setSendingSessionIds((current) => pruneSessionFlags(current, availableSessionIds));
    setStoppingSessionIds((current) => pruneSessionFlags(current, availableSessionIds));
    setKillingSessionIds((current) => pruneSessionFlags(current, availableSessionIds));
    setKillRevealSessionId((current) =>
      current && availableSessionIds.has(current) ? current : null,
    );
    setPendingKillSessionId((current) =>
      current && availableSessionIds.has(current) ? current : null,
    );
    setUpdatingSessionIds((current) => pruneSessionFlags(current, availableSessionIds));
  }

  function adoptState(
    nextState: StateResponse,
    options?: { openSessionId?: string; paneId?: string | null },
  ) {
    setCodexState(nextState.codex ?? {});
    adoptSessions(nextState.sessions, options);
  }


  useEffect(() => {
    let cancelled = false;
    const eventSource = new EventSource("/api/events");

    function requestStateResync() {
      if (cancelled) {
        return;
      }

      stateResyncPendingRef.current = true;
      if (stateResyncInFlightRef.current) {
        return;
      }

      stateResyncInFlightRef.current = true;
      void (async () => {
        try {
          while (!cancelled && stateResyncPendingRef.current) {
            stateResyncPendingRef.current = false;
            const requestEpoch = liveStateEpochRef.current;

            try {
              const state = await fetchState();
              if (cancelled) {
                return;
              }

              if (liveStateEpochRef.current !== requestEpoch) {
                continue;
              }

              adoptState(state);
              setRequestError(null);
            } catch (error) {
              if (!cancelled) {
                setRequestError(getErrorMessage(error));
              }
              break;
            } finally {
              if (!cancelled) {
                setIsLoading(false);
              }
            }
          }
        } finally {
          stateResyncInFlightRef.current = false;
        }
      })();
    }

    function handleStateEvent(event: MessageEvent<string>) {
      if (cancelled) {
        return;
      }

      try {
        const state = JSON.parse(event.data) as StateResponse;
        liveStateEpochRef.current += 1;
        adoptState(state);
        setRequestError(null);
      } catch (error) {
        if (!cancelled) {
          setRequestError(getErrorMessage(error));
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
        liveStateEpochRef.current += 1;

        const result = applyDeltaToSessions(sessionsRef.current, delta);
        if (result.kind === "applied") {
          sessionsRef.current = result.sessions;
          startTransition(() => {
            setSessions(result.sessions);
          });
          setRequestError(null);
          return;
        }

        requestStateResync();
      } catch {
        requestStateResync();
      }
    }

    eventSource.addEventListener("state", handleStateEvent as EventListener);
    eventSource.addEventListener("delta", handleDeltaEvent as EventListener);
    eventSource.onopen = () => {
      if (!cancelled) {
        setRequestError(null);
      }
    };

    requestStateResync();

    return () => {
      cancelled = true;
      eventSource.removeEventListener("state", handleStateEvent as EventListener);
      eventSource.removeEventListener("delta", handleDeltaEvent as EventListener);
      eventSource.close();
    };
  }, []);

  useEffect(() => {
    if (activeSession) {
      setNewSessionAgent(activeSession.agent);
    }
  }, [activeSession?.id]);

  useLayoutEffect(() => {
    applyThemePreference(themeId);
    persistThemePreference(themeId);
  }, [themeId]);

  useEffect(() => {
    sessionsRef.current = sessions;
  }, [sessions]);

  useEffect(() => {
    draftAttachmentsRef.current = draftAttachmentsBySessionId;
  }, [draftAttachmentsBySessionId]);

  useEffect(() => {
    return () => {
      releaseDraftAttachments(Object.values(draftAttachmentsRef.current).flat());
    };
  }, []);

  useEffect(() => {
    if (!pendingKillSessionId) {
      return;
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        setPendingKillSessionId(null);
        return;
      }

      if (event.key === "Enter" && !event.repeat) {
        event.preventDefault();
        void confirmKillSession();
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [pendingKillSessionId]);

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
        0.22,
        0.78,
      );

      setWorkspace((current) => updateSplitRatio(current, resizeState.splitId, nextRatio));
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

  async function handleSend(sessionId: string, draftTextOverride?: string) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return;
    }

    const draftText = draftTextOverride ?? draftsBySessionId[sessionId] ?? "";
    const prompt = draftText.trim();
    const attachments = draftAttachmentsBySessionId[sessionId] ?? [];
    if (!prompt && attachments.length === 0) {
      return;
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

    try {
      const state = await sendMessage(
        sessionId,
        prompt,
        attachments.map((attachment) => ({
          data: attachment.base64Data,
          fileName: attachment.fileName,
          mediaType: attachment.mediaType,
        })),
      );
      adoptState(state);
      releaseDraftAttachments(attachments);
      setRequestError(null);
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
        if (attachments.length === 0 || (current[sessionId]?.length ?? 0) > 0) {
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
      setRequestError(getErrorMessage(error));
    } finally {
      setSendingSessionIds((current) => setSessionFlag(current, sessionId, false));
    }
  }

  function handleDraftAttachmentsAdd(sessionId: string, attachments: DraftImageAttachment[]) {
    setDraftAttachmentsBySessionId((current) => ({
      ...current,
      [sessionId]: [...(current[sessionId] ?? []), ...attachments],
    }));
  }

  function handleDraftAttachmentRemove(sessionId: string, attachmentId: string) {
    setDraftAttachmentsBySessionId((current) => {
      const existing = current[sessionId];
      if (!existing) {
        return current;
      }

      const removed = existing.filter((attachment) => attachment.id === attachmentId);
      if (removed.length === 0) {
        return current;
      }

      releaseDraftAttachments(removed);
      const nextAttachments = existing.filter((attachment) => attachment.id !== attachmentId);
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

  async function handleNewSession() {
    setIsCreating(true);
    try {
      const created = await createSession({
        agent: newSessionAgent,
        approvalPolicy:
          newSessionAgent === "Codex" ? defaultCodexApprovalPolicy : undefined,
        claudeApprovalMode:
          newSessionAgent === "Claude" ? defaultClaudeApprovalMode : undefined,
        sandboxMode: newSessionAgent === "Codex" ? defaultCodexSandboxMode : undefined,
        workdir: activeSession?.workdir,
      });
      const state = await fetchState();
      adoptState(state, {
        openSessionId: created.id,
        paneId: workspace.activePaneId,
      });
      setRequestError(null);
    } catch (error) {
      setRequestError(getErrorMessage(error));
    } finally {
      setIsCreating(false);
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
      setRequestError(getErrorMessage(error));
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
      setRequestError(getErrorMessage(error));
    }
  }

  async function handleStopSession(sessionId: string) {
    setStoppingSessionIds((current) => setSessionFlag(current, sessionId, true));
    try {
      const state = await stopSession(sessionId);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      setRequestError(getErrorMessage(error));
    } finally {
      setStoppingSessionIds((current) => setSessionFlag(current, sessionId, false));
    }
  }

  async function handleKillSession(sessionId: string) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return;
    }

    setPendingKillSessionId(sessionId);
  }

  async function confirmKillSession() {
    if (!pendingKillSessionId) {
      return;
    }

    const sessionId = pendingKillSessionId;
    setPendingKillSessionId(null);
    setKillRevealSessionId(null);

    setKillingSessionIds((current) => setSessionFlag(current, sessionId, true));
    try {
      const state = await killSession(sessionId);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      setRequestError(getErrorMessage(error));
    } finally {
      setKillingSessionIds((current) => setSessionFlag(current, sessionId, false));
    }
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

    setUpdatingSessionIds((current) => setSessionFlag(current, sessionId, true));
    try {
      const payload =
        session.agent === "Codex"
          ? {
              sandboxMode:
                field === "sandboxMode"
                  ? (value as SandboxMode)
                  : (session.sandboxMode ?? "workspace-write"),
              approvalPolicy:
                field === "approvalPolicy"
                  ? (value as ApprovalPolicy)
                  : (session.approvalPolicy ?? "never"),
            }
          : field === "claudeApprovalMode"
            ? {
                claudeApprovalMode: value as ClaudeApprovalMode,
              }
            : null;
      if (!payload) {
        return;
      }

      const state = await updateSessionSettings(sessionId, payload);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      setRequestError(getErrorMessage(error));
    } finally {
      setUpdatingSessionIds((current) => setSessionFlag(current, sessionId, false));
    }
  }

  function handleSidebarSessionClick(sessionId: string) {
    setKillRevealSessionId(null);
    setPendingScrollToBottomRequest({
      sessionId,
      token: Date.now(),
    });
    setWorkspace((current) => openSessionInWorkspaceState(current, sessionId, current.activePaneId));
  }

  function handleScrollToBottomRequestHandled(token: number) {
    setPendingScrollToBottomRequest((current) =>
      current?.token === token ? null : current,
    );
  }

  const pendingKillSession = pendingKillSessionId
    ? (sessionLookup.get(pendingKillSessionId) ?? null)
    : null;

  function handlePaneActivate(paneId: string) {
    setWorkspace((current) => activatePane(current, paneId));
  }

  function handlePaneSessionSelect(paneId: string, sessionId: string) {
    setWorkspace((current) => activatePane(current, paneId, sessionId));
  }

  function handleCloseTab(paneId: string, sessionId: string) {
    setWorkspace((current) => closeSessionTab(current, paneId, sessionId));
  }

  function handleSplitPane(paneId: string, direction: "row" | "column") {
    setWorkspace((current) => splitPane(current, paneId, direction));
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
    resizeStateRef.current = {
      splitId,
      direction,
      startRatio: ratio,
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

  function handleTabDragStart(sourcePaneId: string, sessionId: string) {
    window.requestAnimationFrame(() => {
      setDraggedTab({
        sourcePaneId,
        sessionId,
      });
    });
  }

  function handleTabDragEnd() {
    setDraggedTab(null);
  }

  function handleTabDrop(targetPaneId: string, placement: TabDropPlacement, tabIndex?: number) {
    if (!draggedTab) {
      return;
    }

    const drop = draggedTab;
    setDraggedTab(null);
    startTransition(() => {
      setWorkspace((current) =>
        placeDraggedSession(
          current,
          drop.sourcePaneId,
          drop.sessionId,
          targetPaneId,
          placement,
          tabIndex,
        ),
      );
    });
  }

  function handlePaneViewModeChange(paneId: string, viewMode: PaneViewMode) {
    setWorkspace((current) => setPaneViewMode(current, paneId, viewMode));
  }

  function handlePaneSourcePathChange(paneId: string, path: string) {
    setWorkspace((current) => setPaneSourcePath(current, paneId, path));
  }

  return (
    <div className="shell">
      <div className="background-orbit background-orbit-left" />
      <div className="background-orbit background-orbit-right" />

      <aside className="sidebar panel">
        <div className="brand-block">
          <p className="eyebrow">Terminal meets AI</p>
          <h1>TermAl</h1>
          <p className="brand-copy">
            A local control room for AI coding sessions with semantic cards instead of raw terminal
            noise.
          </p>
        </div>

        <div className="new-session-controls">
          <label className="session-control-label" htmlFor="new-session-agent">
            New session
          </label>
          <div className="new-session-row">
            <ThemedCombobox
              id="new-session-agent"
              className="new-session-agent-select"
              value={newSessionAgent}
              options={NEW_SESSION_AGENT_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => setNewSessionAgent(nextValue as AgentType)}
              disabled={isCreating}
            />
            <button
              className="new-session-button"
              type="button"
              onClick={handleNewSession}
              disabled={isCreating}
            >
              {isCreating ? "Creating..." : "New Session"}
            </button>
          </div>
        </div>

        <button
          className="settings-launcher"
          type="button"
          onClick={() => setIsSettingsOpen(true)}
          aria-haspopup="dialog"
          aria-expanded={isSettingsOpen}
          aria-controls="settings-dialog"
        >
            <span className="settings-launcher-copy">
              <span className="session-control-label">Settings</span>
              <strong className="settings-launcher-title">Open preferences</strong>
              <span className="settings-launcher-description">
                Appearance, themes, and Claude session defaults.
              </span>
            </span>
            <span className="settings-launcher-badge">{activeTheme.name}</span>
        </button>

        <div className="session-list">
          {filteredSessions.length > 0 ? (
            filteredSessions.map((session) => {
              const isActive = session.id === activeSession?.id;
              const isOpen = openSessionIds.has(session.id);
              const isKilling = Boolean(killingSessionIds[session.id]);
              const isKillVisible = isKilling || killRevealSessionId === session.id;

              return (
                <div
                  key={session.id}
                  className={`session-row-shell ${isActive ? "selected" : ""} ${isOpen ? "open" : ""} ${isKillVisible ? "kill-armed" : ""}`}
                  onMouseLeave={() => {
                    if (!isKilling) {
                      setKillRevealSessionId((current) => (current === session.id ? null : current));
                    }
                  }}
                  onBlur={(event) => {
                    const nextTarget = event.relatedTarget;
                    if (
                      !isKilling &&
                      (!(nextTarget instanceof Node) || !event.currentTarget.contains(nextTarget))
                    ) {
                      setKillRevealSessionId((current) => (current === session.id ? null : current));
                    }
                  }}
                >
                  <button
                    className={`session-row ${isActive ? "selected" : ""} ${isOpen ? "open" : ""}`}
                    type="button"
                    onClick={() => handleSidebarSessionClick(session.id)}
                  >
                    <div className="session-avatar">{session.emoji}</div>
                    <div className="session-copy">
                      <div className="session-title-line">
                        <strong>{session.name}</strong>
                      </div>
                      <div className="session-meta">
                        {session.agent} <span className="meta-separator">/</span> {session.workdir}
                      </div>
                      <div className="session-preview">{session.preview}</div>
                    </div>
                  </button>
                  <button
                    className="session-row-status-button"
                    type="button"
                    onClick={() =>
                      setKillRevealSessionId((current) =>
                        current === session.id && !isKilling ? null : session.id,
                      )
                    }
                    aria-label={`Show session actions for ${session.name}`}
                  >
                    <span className={`status-dot status-${session.status}`} />
                  </button>
                  <button
                    className="ghost-button session-row-kill"
                    type="button"
                    onClick={() => void handleKillSession(session.id)}
                    disabled={isKilling}
                    aria-label={`Kill ${session.name}`}
                  >
                    {isKilling ? "Killing" : "Kill"}
                  </button>
                </div>
              );
            })
          ) : (
            <div className="session-filter-empty">
              {sessions.length === 0 ? "No sessions yet." : "No sessions match this filter."}
            </div>
          )}
        </div>

        <section className="sidebar-status" aria-label="Workspace status">
          <div className="session-control-label">Status</div>
          <div className="sidebar-status-chips">
            <button
              className={`chip sidebar-status-chip ${sessionListFilter === "all" ? "selected" : ""}`}
              type="button"
              onClick={() => setSessionListFilter("all")}
              aria-pressed={sessionListFilter === "all"}
            >
              No filter ({sessionFilterCounts.all})
            </button>
            <button
              className={`chip sidebar-status-chip ${sessionListFilter === "working" ? "selected" : ""}`}
              type="button"
              onClick={() => setSessionListFilter("working")}
              aria-pressed={sessionListFilter === "working"}
            >
              Working ({sessionFilterCounts.working})
            </button>
            <button
              className={`chip sidebar-status-chip ${sessionListFilter === "asking" ? "selected" : ""}`}
              type="button"
              onClick={() => setSessionListFilter("asking")}
              aria-pressed={sessionListFilter === "asking"}
            >
              Asking ({sessionFilterCounts.asking})
            </button>
            <button
              className={`chip sidebar-status-chip ${sessionListFilter === "completed" ? "selected" : ""}`}
              type="button"
              onClick={() => setSessionListFilter("completed")}
              aria-pressed={sessionListFilter === "completed"}
            >
              Completed ({sessionFilterCounts.completed})
            </button>
          </div>
        </section>
      </aside>

      <main className="workspace-shell">
        {requestError ? (
          <article className="thread-notice workspace-notice">
            <div className="card-label">Backend</div>
            <p>{requestError}</p>
          </article>
        ) : null}

        <section className="workspace-stage">
          {workspace.root ? (
            <WorkspaceNodeView
              node={workspace.root}
              codexState={codexState}
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
              paneShouldStickToBottomRef={paneShouldStickToBottomRef}
              paneScrollPositionsRef={paneScrollPositionsRef}
              paneContentSignaturesRef={paneContentSignaturesRef}
              pendingScrollToBottomRequest={pendingScrollToBottomRequest}
              draggedTab={draggedTab}
              onActivatePane={handlePaneActivate}
              onSelectSession={handlePaneSessionSelect}
              onCloseTab={handleCloseTab}
              onSplitPane={handleSplitPane}
              onResizeStart={handleSplitResizeStart}
              onTabDragStart={handleTabDragStart}
              onTabDragEnd={handleTabDragEnd}
              onTabDrop={handleTabDrop}
              onPaneViewModeChange={handlePaneViewModeChange}
              onPaneSourcePathChange={handlePaneSourcePathChange}
              onDraftCommit={handleDraftChange}
              onDraftAttachmentsAdd={handleDraftAttachmentsAdd}
              onDraftAttachmentRemove={handleDraftAttachmentRemove}
              onComposerError={setRequestError}
              onSend={handleSend}
              onCancelQueuedPrompt={handleCancelQueuedPrompt}
              onApprovalDecision={handleApprovalDecision}
              onStopSession={handleStopSession}
              onKillSession={handleKillSession}
              onScrollToBottomRequestHandled={handleScrollToBottomRequestHandled}
              onSessionSettingsChange={handleSessionSettingsChange}
            />
          ) : (
            <div className="workspace-empty panel">
              <EmptyState
                title={isLoading ? "Connecting to backend" : "No sessions in the workspace"}
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

      {pendingKillSession ? (
        <div
          className="dialog-backdrop"
          onMouseDown={() => {
            setPendingKillSessionId(null);
          }}
        >
          <section
            className="dialog-card panel"
            role="dialog"
            aria-modal="true"
            aria-labelledby="kill-dialog-title"
            onMouseDown={(event) => {
              event.stopPropagation();
            }}
          >
            <div className="card-label">Confirm</div>
            <h2 id="kill-dialog-title">Kill {pendingKillSession.name}?</h2>
            <p className="dialog-copy">
              The current process will stop and the session will be removed from the list.
            </p>
            <div className="dialog-actions">
              <button
                className="ghost-button"
                type="button"
                onClick={() => {
                  setPendingKillSessionId(null);
                }}
              >
                Cancel
              </button>
              <button className="send-button dialog-danger-button" type="button" onClick={() => void confirmKillSession()}>
                Kill Session
              </button>
            </div>
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
                  Tune the interface without disturbing active sessions.
                </p>
              </div>

              <button
                className="ghost-button settings-dialog-close"
                type="button"
                onClick={() => {
                  setIsSettingsOpen(false);
                }}
              >
                Close
              </button>
            </div>

            <div className="settings-dialog-body">
              <div className="settings-tab-list" role="tablist" aria-label="Preferences sections">
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
                  <ThemePicker
                    activeTheme={activeTheme}
                    themeId={themeId}
                    onSelectTheme={setThemeId}
                  />
                ) : settingsTab === "codex-prompts" ? (
                  <CodexPromptPreferencesPanel
                    defaultApprovalPolicy={defaultCodexApprovalPolicy}
                    defaultSandboxMode={defaultCodexSandboxMode}
                    onSelectApprovalPolicy={setDefaultCodexApprovalPolicy}
                    onSelectSandboxMode={setDefaultCodexSandboxMode}
                  />
                ) : (
                  <ClaudeApprovalsPreferencesPanel
                    defaultClaudeApprovalMode={defaultClaudeApprovalMode}
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

function ThemePicker({
  activeTheme,
  themeId,
  onSelectTheme,
}: {
  activeTheme: (typeof THEMES)[number];
  themeId: ThemeId;
  onSelectTheme: (themeId: ThemeId) => void;
}) {
  const listRef = useRef<HTMLDivElement | null>(null);
  const dragStateRef = useRef<{
    pointerId: number;
    startPointerY: number;
    startThumbOffset: number;
  } | null>(null);
  const [scrollState, setScrollState] = useState({
    thumbHeight: 0,
    thumbOffset: 0,
    visible: false,
  });
  const [isDraggingScrollbar, setIsDraggingScrollbar] = useState(false);

  useLayoutEffect(() => {
    const listElement = listRef.current;
    if (!listElement) {
      return;
    }

    const updateScrollState = () => {
      const { clientHeight, scrollHeight, scrollTop } = listElement;
      const hasOverflow = scrollHeight - clientHeight > 1;

      if (!hasOverflow) {
        setScrollState((current) =>
          current.visible || current.thumbHeight !== 0 || current.thumbOffset !== 0
            ? { thumbHeight: 0, thumbOffset: 0, visible: false }
            : current,
        );
        return;
      }

      const thumbHeight = Math.max((clientHeight * clientHeight) / scrollHeight, 48);
      const maxThumbOffset = Math.max(clientHeight - thumbHeight, 0);
      const maxScrollTop = Math.max(scrollHeight - clientHeight, 1);
      const thumbOffset = (scrollTop / maxScrollTop) * maxThumbOffset;

      setScrollState((current) => {
        if (
          current.visible &&
          Math.abs(current.thumbHeight - thumbHeight) < 0.5 &&
          Math.abs(current.thumbOffset - thumbOffset) < 0.5
        ) {
          return current;
        }

        return {
          thumbHeight,
          thumbOffset,
          visible: true,
        };
      });
    };

    updateScrollState();

    listElement.addEventListener("scroll", updateScrollState);
    window.addEventListener("resize", updateScrollState);

    const resizeObserver =
      typeof ResizeObserver === "undefined"
        ? null
        : new ResizeObserver(() => {
            updateScrollState();
          });

    resizeObserver?.observe(listElement);

    return () => {
      listElement.removeEventListener("scroll", updateScrollState);
      window.removeEventListener("resize", updateScrollState);
      resizeObserver?.disconnect();
    };
  }, []);

  useEffect(() => {
    function handlePointerMove(event: PointerEvent) {
      const dragState = dragStateRef.current;
      const listElement = listRef.current;
      if (!dragState || !listElement || event.pointerId !== dragState.pointerId) {
        return;
      }

      const { clientHeight, scrollHeight } = listElement;
      const thumbHeight = Math.max((clientHeight * clientHeight) / Math.max(scrollHeight, 1), 48);
      const maxThumbOffset = Math.max(clientHeight - thumbHeight, 0);
      const nextThumbOffset = clamp(
        dragState.startThumbOffset + (event.clientY - dragState.startPointerY),
        0,
        maxThumbOffset,
      );
      const maxScrollTop = Math.max(scrollHeight - clientHeight, 0);

      listElement.scrollTop =
        maxThumbOffset > 0 ? (nextThumbOffset / maxThumbOffset) * maxScrollTop : 0;
    }

    function handlePointerUp(event: PointerEvent) {
      const dragState = dragStateRef.current;
      if (!dragState || event.pointerId !== dragState.pointerId) {
        return;
      }

      dragStateRef.current = null;
      setIsDraggingScrollbar(false);
    }

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
    window.addEventListener("pointercancel", handlePointerUp);

    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
      window.removeEventListener("pointercancel", handlePointerUp);
    };
  }, []);

  function scrollListToThumbOffset(nextThumbOffset: number) {
    const listElement = listRef.current;
    if (!listElement) {
      return;
    }

    const { clientHeight, scrollHeight } = listElement;
    const thumbHeight = Math.max((clientHeight * clientHeight) / Math.max(scrollHeight, 1), 48);
    const maxThumbOffset = Math.max(clientHeight - thumbHeight, 0);
    const clampedThumbOffset = clamp(nextThumbOffset, 0, maxThumbOffset);
    const maxScrollTop = Math.max(scrollHeight - clientHeight, 0);

    listElement.scrollTop =
      maxThumbOffset > 0 ? (clampedThumbOffset / maxThumbOffset) * maxScrollTop : 0;
  }

  function handleScrollbarThumbPointerDown(event: ReactPointerEvent<HTMLSpanElement>) {
    event.preventDefault();
    event.stopPropagation();

    dragStateRef.current = {
      pointerId: event.pointerId,
      startPointerY: event.clientY,
      startThumbOffset: scrollState.thumbOffset,
    };
    setIsDraggingScrollbar(true);
  }

  function handleScrollbarTrackPointerDown(event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault();

    const trackRect = event.currentTarget.getBoundingClientRect();
    const nextThumbOffset = event.clientY - trackRect.top - scrollState.thumbHeight / 2;
    const { clientHeight } = event.currentTarget;
    const clampedThumbOffset = clamp(
      nextThumbOffset,
      0,
      Math.max(clientHeight - scrollState.thumbHeight, 0),
    );

    scrollListToThumbOffset(clampedThumbOffset);
    dragStateRef.current = {
      pointerId: event.pointerId,
      startPointerY: event.clientY,
      startThumbOffset: clampedThumbOffset,
    };
    setIsDraggingScrollbar(true);
  }

  return (
    <section className="theme-panel">
      <div className="theme-panel-header">
        <div>
          <p className="session-control-label">Appearance</p>
          <p className="theme-panel-copy">{activeTheme.description}</p>
        </div>
        <span className="theme-active-badge">{activeTheme.name}</span>
      </div>

      <div className="theme-option-list-shell">
        <div ref={listRef} className="theme-option-list" role="radiogroup" aria-label="UI theme">
          {THEMES.map((theme) => {
            const isSelected = theme.id === themeId;

            return (
              <button
                key={theme.id}
                className={`theme-option ${isSelected ? "selected" : ""}`}
                type="button"
                role="radio"
                aria-checked={isSelected}
                onClick={() => onSelectTheme(theme.id)}
              >
                <span className="theme-option-main">
                  <span className="theme-option-title-row">
                    <strong className="theme-option-title">{theme.name}</strong>
                    {isSelected ? <span className="theme-option-status">Live</span> : null}
                  </span>
                  <span className="theme-option-copy">{theme.description}</span>
                </span>
                <span className="theme-option-preview" aria-hidden="true">
                  {theme.swatches.map((swatch) => (
                    <span
                      key={`${theme.id}-${swatch}`}
                      className="theme-option-swatch"
                      style={{ background: swatch }}
                    />
                  ))}
                </span>
              </button>
            );
          })}
        </div>
        {scrollState.visible ? (
          <div
            className={`theme-option-scrollbar ${isDraggingScrollbar ? "dragging" : ""}`}
            aria-hidden="true"
            onPointerDown={handleScrollbarTrackPointerDown}
          >
            <span
              className="theme-option-scrollbar-thumb"
              onPointerDown={handleScrollbarThumbPointerDown}
              style={{
                height: `${scrollState.thumbHeight}px`,
                transform: `translateY(${scrollState.thumbOffset}px)`,
              }}
            />
          </div>
        ) : null}
      </div>
    </section>
  );
}

function ClaudeApprovalsPreferencesPanel({
  defaultClaudeApprovalMode,
  onSelectMode,
}: {
  defaultClaudeApprovalMode: ClaudeApprovalMode;
  onSelectMode: (mode: ClaudeApprovalMode) => void;
}) {
  return (
    <section className="settings-panel-stack">
      <div className="settings-panel-intro">
        <div>
          <p className="session-control-label">New Claude sessions</p>
          <p className="settings-panel-copy">
            Choose the default approval mode for Claude sessions created in this window.
          </p>
        </div>
      </div>

      <article className="message-card prompt-settings-card">
        <div className="card-label">Session Default</div>
        <h3>Claude approvals</h3>
        <div className="prompt-settings-grid">
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-claude-approval-mode">
              Default approval mode
            </label>
            <ThemedCombobox
              id="default-claude-approval-mode"
              className="prompt-settings-select"
              value={defaultClaudeApprovalMode}
              options={CLAUDE_APPROVAL_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectMode(nextValue as ClaudeApprovalMode)}
            />
          </div>
          <p className="session-control-hint">
            This only affects new Claude sessions you create here. Existing sessions keep their
            current approval mode.
          </p>
        </div>
      </article>
    </section>
  );
}

function CodexPromptPreferencesPanel({
  defaultApprovalPolicy,
  defaultSandboxMode,
  onSelectApprovalPolicy,
  onSelectSandboxMode,
}: {
  defaultApprovalPolicy: ApprovalPolicy;
  defaultSandboxMode: SandboxMode;
  onSelectApprovalPolicy: (policy: ApprovalPolicy) => void;
  onSelectSandboxMode: (mode: SandboxMode) => void;
}) {
  return (
    <section className="settings-panel-stack">
      <div className="settings-panel-intro">
        <div>
          <p className="session-control-label">New Codex sessions</p>
          <p className="settings-panel-copy">
            Choose the default prompt sandbox and approval policy for Codex sessions created in
            this window.
          </p>
        </div>
      </div>

      <article className="message-card prompt-settings-card">
        <div className="card-label">Session Default</div>
        <h3>Codex prompt settings</h3>
        <div className="prompt-settings-grid">
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-codex-sandbox-mode">
              Default sandbox
            </label>
            <ThemedCombobox
              id="default-codex-sandbox-mode"
              className="prompt-settings-select"
              value={defaultSandboxMode}
              options={SANDBOX_MODE_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectSandboxMode(nextValue as SandboxMode)}
            />
          </div>
          <div className="session-control-group">
            <label className="session-control-label" htmlFor="default-codex-approval-policy">
              Default approval policy
            </label>
            <ThemedCombobox
              id="default-codex-approval-policy"
              className="prompt-settings-select"
              value={defaultApprovalPolicy}
              options={APPROVAL_POLICY_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => onSelectApprovalPolicy(nextValue as ApprovalPolicy)}
            />
          </div>
          <p className="session-control-hint">
            This only affects new Codex sessions you create here. Existing sessions keep their
            current prompt settings.
          </p>
        </div>
      </article>
    </section>
  );
}

function ThemedCombobox({
  className,
  disabled = false,
  id,
  onChange,
  options,
  value,
}: {
  className?: string;
  disabled?: boolean;
  id: string;
  onChange: (nextValue: string) => void;
  options: readonly ComboboxOption[];
  value: string;
}) {
  const listboxId = useId();
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  const [isOpen, setIsOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(() =>
    Math.max(
      options.findIndex((option) => option.value === value),
      0,
    ),
  );
  const [menuStyle, setMenuStyle] = useState<CSSProperties | null>(null);

  const selectedIndex = options.findIndex((option) => option.value === value);
  const safeSelectedIndex = selectedIndex >= 0 ? selectedIndex : 0;
  const selectedOption = options[safeSelectedIndex] ?? options[0];

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    setActiveIndex(safeSelectedIndex);
  }, [isOpen, safeSelectedIndex]);

  useLayoutEffect(() => {
    if (!isOpen) {
      return;
    }

    function updateMenuStyle() {
      const trigger = triggerRef.current;
      if (!trigger) {
        return;
      }

      const rect = trigger.getBoundingClientRect();
      const viewportPadding = 12;
      const estimatedHeight = Math.min(Math.max(options.length * 52 + 12, 120), 280);
      const availableBelow = window.innerHeight - rect.bottom - viewportPadding;
      const availableAbove = rect.top - viewportPadding;
      const openUpward =
        availableBelow < Math.min(estimatedHeight, 220) && availableAbove > availableBelow;
      const maxHeight = Math.max(openUpward ? availableAbove : availableBelow, 140);

      setMenuStyle({
        left: rect.left,
        width: rect.width,
        maxHeight,
        top: openUpward ? undefined : rect.bottom + 8,
        bottom: openUpward ? window.innerHeight - rect.top + 8 : undefined,
      });
    }

    updateMenuStyle();
    window.addEventListener("resize", updateMenuStyle);
    window.addEventListener("scroll", updateMenuStyle, true);

    return () => {
      window.removeEventListener("resize", updateMenuStyle);
      window.removeEventListener("scroll", updateMenuStyle, true);
    };
  }, [isOpen, options.length]);

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    function handlePointerDown(event: PointerEvent) {
      const target = event.target;
      if (!(target instanceof Node)) {
        return;
      }

      if (triggerRef.current?.contains(target) || listRef.current?.contains(target)) {
        return;
      }

      setIsOpen(false);
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        setIsOpen(false);
        triggerRef.current?.focus();
        return;
      }

      if (event.key === "Tab") {
        setIsOpen(false);
        return;
      }

      if (event.key === "ArrowDown") {
        event.preventDefault();
        setActiveIndex((current) => (current + 1) % options.length);
        return;
      }

      if (event.key === "ArrowUp") {
        event.preventDefault();
        setActiveIndex((current) => (current - 1 + options.length) % options.length);
        return;
      }

      if (event.key === "Home") {
        event.preventDefault();
        setActiveIndex(0);
        return;
      }

      if (event.key === "End") {
        event.preventDefault();
        setActiveIndex(options.length - 1);
        return;
      }

      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        const nextOption = options[activeIndex];
        if (!nextOption) {
          return;
        }

        onChange(nextOption.value);
        setIsOpen(false);
        triggerRef.current?.focus();
      }
    }

    document.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);

    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [activeIndex, isOpen, onChange, options]);

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    const activeOption = listRef.current?.querySelector<HTMLElement>(
      `[data-option-index="${activeIndex}"]`,
    );
    activeOption?.scrollIntoView({
      block: "nearest",
    });
  }, [activeIndex, isOpen]);

  function handleTriggerKeyDown(event: ReactKeyboardEvent<HTMLButtonElement>) {
    if (disabled) {
      return;
    }

    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setActiveIndex(safeSelectedIndex);
      setIsOpen(true);
      return;
    }

    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      setActiveIndex(safeSelectedIndex);
      setIsOpen((current) => !current);
    }
  }

  return (
    <>
      <button
        ref={triggerRef}
        id={id}
        className={`session-select combo-trigger ${className ?? ""}`.trim()}
        type="button"
        role="combobox"
        aria-controls={listboxId}
        aria-expanded={isOpen}
        aria-haspopup="listbox"
        aria-activedescendant={isOpen ? `${listboxId}-option-${activeIndex}` : undefined}
        disabled={disabled}
        onClick={() => {
          if (!disabled) {
            setActiveIndex(safeSelectedIndex);
            setIsOpen((current) => !current);
          }
        }}
        onKeyDown={handleTriggerKeyDown}
      >
        <span className="combo-trigger-value">{selectedOption?.label ?? value}</span>
        <span className={`combo-trigger-caret ${isOpen ? "open" : ""}`} aria-hidden="true">
          v
        </span>
      </button>

      {isOpen && menuStyle
        ? createPortal(
            <div
              ref={listRef}
              id={listboxId}
              className="combo-menu"
              role="listbox"
              style={menuStyle}
            >
              {options.map((option, index) => {
                const isSelected = option.value === value;
                const isActive = index === activeIndex;

                return (
                  <button
                    key={option.value}
                    id={`${listboxId}-option-${index}`}
                    className={`combo-option ${isActive ? "active" : ""} ${isSelected ? "selected" : ""}`}
                    type="button"
                    role="option"
                    aria-selected={isSelected}
                    data-option-index={index}
                    onMouseEnter={() => {
                      setActiveIndex(index);
                    }}
                    onClick={() => {
                      onChange(option.value);
                      setIsOpen(false);
                      triggerRef.current?.focus();
                    }}
                  >
                    <span className="combo-option-label">{option.label}</span>
                    <span
                      className={`combo-option-indicator ${isSelected ? "visible" : ""}`}
                      aria-hidden="true"
                    />
                  </button>
                );
              })}
            </div>,
            document.body,
          )
        : null}
    </>
  );
}

function WorkspaceNodeView({
  node,
  codexState,
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
  paneShouldStickToBottomRef,
  paneScrollPositionsRef,
  paneContentSignaturesRef,
  pendingScrollToBottomRequest,
  draggedTab,
  onActivatePane,
  onSelectSession,
  onCloseTab,
  onSplitPane,
  onResizeStart,
  onTabDragStart,
  onTabDragEnd,
  onTabDrop,
  onPaneViewModeChange,
  onPaneSourcePathChange,
  onDraftCommit,
  onDraftAttachmentsAdd,
  onDraftAttachmentRemove,
  onComposerError,
  onSend,
  onCancelQueuedPrompt,
  onApprovalDecision,
  onStopSession,
  onKillSession,
  onScrollToBottomRequestHandled,
  onSessionSettingsChange,
}: {
  node: WorkspaceNode;
  codexState: CodexState;
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
  paneShouldStickToBottomRef: React.MutableRefObject<Record<string, boolean | undefined>>;
  paneScrollPositionsRef: React.MutableRefObject<
    Record<string, Record<string, { top: number; shouldStick: boolean }>>
  >;
  paneContentSignaturesRef: React.MutableRefObject<Record<string, Record<string, string>>>;
  pendingScrollToBottomRequest: {
    sessionId: string;
    token: number;
  } | null;
  draggedTab: {
    sourcePaneId: string;
    sessionId: string;
  } | null;
  onActivatePane: (paneId: string) => void;
  onSelectSession: (paneId: string, sessionId: string) => void;
  onCloseTab: (paneId: string, sessionId: string) => void;
  onSplitPane: (paneId: string, direction: "row" | "column") => void;
  onResizeStart: (
    splitId: string,
    direction: "row" | "column",
    event: ReactPointerEvent<HTMLDivElement>,
  ) => void;
  onTabDragStart: (sourcePaneId: string, sessionId: string) => void;
  onTabDragEnd: () => void;
  onTabDrop: (targetPaneId: string, placement: TabDropPlacement, tabIndex?: number) => void;
  onPaneViewModeChange: (paneId: string, viewMode: PaneViewMode) => void;
  onPaneSourcePathChange: (paneId: string, path: string) => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentsAdd: (sessionId: string, attachments: DraftImageAttachment[]) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onComposerError: (message: string | null) => void;
  onSend: (sessionId: string, draftText?: string) => void;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onKillSession: (sessionId: string) => void;
  onScrollToBottomRequestHandled: (token: number) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
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
        sessions={pane.sessionIds
          .map((sessionId) => sessionLookup.get(sessionId))
          .filter((session): session is Session => session !== undefined)}
        isActive={pane.id === activePaneId}
        isLoading={isLoading}
        draft={pane.activeSessionId ? draftsBySessionId[pane.activeSessionId] ?? "" : ""}
        draftAttachments={
          pane.activeSessionId ? draftAttachmentsBySessionId[pane.activeSessionId] ?? [] : []
        }
        isSending={pane.activeSessionId ? Boolean(sendingSessionIds[pane.activeSessionId]) : false}
        isStopping={pane.activeSessionId ? Boolean(stoppingSessionIds[pane.activeSessionId]) : false}
        isKilling={pane.activeSessionId ? Boolean(killingSessionIds[pane.activeSessionId]) : false}
        isUpdating={pane.activeSessionId ? Boolean(updatingSessionIds[pane.activeSessionId]) : false}
        paneShouldStickToBottomRef={paneShouldStickToBottomRef}
        paneScrollPositionsRef={paneScrollPositionsRef}
        paneContentSignaturesRef={paneContentSignaturesRef}
        pendingScrollToBottomRequest={pendingScrollToBottomRequest}
        draggedTab={draggedTab}
        onActivatePane={onActivatePane}
        onSelectSession={onSelectSession}
        onCloseTab={onCloseTab}
        onSplitPane={onSplitPane}
        onTabDragStart={onTabDragStart}
        onTabDragEnd={onTabDragEnd}
        onTabDrop={onTabDrop}
        onPaneViewModeChange={onPaneViewModeChange}
        onPaneSourcePathChange={onPaneSourcePathChange}
        onDraftCommit={onDraftCommit}
        onDraftAttachmentsAdd={onDraftAttachmentsAdd}
        onDraftAttachmentRemove={onDraftAttachmentRemove}
        onComposerError={onComposerError}
        onSend={onSend}
        onCancelQueuedPrompt={onCancelQueuedPrompt}
        onApprovalDecision={onApprovalDecision}
        onStopSession={onStopSession}
        onKillSession={onKillSession}
        onScrollToBottomRequestHandled={onScrollToBottomRequestHandled}
        onSessionSettingsChange={onSessionSettingsChange}
      />
    );
  }

  return (
    <div className={`tile-split tile-split-${node.direction}`}>
      <div className="tile-branch" style={{ flexGrow: node.ratio, flexBasis: 0 }}>
        <WorkspaceNodeView
          node={node.first}
          codexState={codexState}
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
          paneShouldStickToBottomRef={paneShouldStickToBottomRef}
          paneScrollPositionsRef={paneScrollPositionsRef}
          paneContentSignaturesRef={paneContentSignaturesRef}
          pendingScrollToBottomRequest={pendingScrollToBottomRequest}
          draggedTab={draggedTab}
          onActivatePane={onActivatePane}
          onSelectSession={onSelectSession}
          onCloseTab={onCloseTab}
          onSplitPane={onSplitPane}
          onResizeStart={onResizeStart}
          onTabDragStart={onTabDragStart}
          onTabDragEnd={onTabDragEnd}
          onTabDrop={onTabDrop}
          onPaneViewModeChange={onPaneViewModeChange}
          onPaneSourcePathChange={onPaneSourcePathChange}
          onDraftCommit={onDraftCommit}
          onDraftAttachmentsAdd={onDraftAttachmentsAdd}
          onDraftAttachmentRemove={onDraftAttachmentRemove}
          onComposerError={onComposerError}
          onSend={onSend}
          onCancelQueuedPrompt={onCancelQueuedPrompt}
          onApprovalDecision={onApprovalDecision}
          onStopSession={onStopSession}
          onKillSession={onKillSession}
          onScrollToBottomRequestHandled={onScrollToBottomRequestHandled}
          onSessionSettingsChange={onSessionSettingsChange}
        />
      </div>

      <div
        className={`tile-divider tile-divider-${node.direction}`}
        onPointerDown={(event) => onResizeStart(node.id, node.direction, event)}
      />

      <div className="tile-branch" style={{ flexGrow: 1 - node.ratio, flexBasis: 0 }}>
        <WorkspaceNodeView
          node={node.second}
          codexState={codexState}
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
          paneShouldStickToBottomRef={paneShouldStickToBottomRef}
          paneScrollPositionsRef={paneScrollPositionsRef}
          paneContentSignaturesRef={paneContentSignaturesRef}
          pendingScrollToBottomRequest={pendingScrollToBottomRequest}
          draggedTab={draggedTab}
          onActivatePane={onActivatePane}
          onSelectSession={onSelectSession}
          onCloseTab={onCloseTab}
          onSplitPane={onSplitPane}
          onResizeStart={onResizeStart}
          onTabDragStart={onTabDragStart}
          onTabDragEnd={onTabDragEnd}
          onTabDrop={onTabDrop}
          onPaneViewModeChange={onPaneViewModeChange}
          onPaneSourcePathChange={onPaneSourcePathChange}
          onDraftCommit={onDraftCommit}
          onDraftAttachmentsAdd={onDraftAttachmentsAdd}
          onDraftAttachmentRemove={onDraftAttachmentRemove}
          onComposerError={onComposerError}
          onSend={onSend}
          onCancelQueuedPrompt={onCancelQueuedPrompt}
          onApprovalDecision={onApprovalDecision}
          onStopSession={onStopSession}
          onKillSession={onKillSession}
          onScrollToBottomRequestHandled={onScrollToBottomRequestHandled}
          onSessionSettingsChange={onSessionSettingsChange}
        />
      </div>
    </div>
  );
}

function SessionPaneView({
  pane,
  codexState,
  sessions,
  isActive,
  isLoading,
  draft,
  draftAttachments,
  isSending,
  isStopping,
  isKilling,
  isUpdating,
  paneShouldStickToBottomRef,
  paneScrollPositionsRef,
  paneContentSignaturesRef,
  pendingScrollToBottomRequest,
  draggedTab,
  onActivatePane,
  onSelectSession,
  onCloseTab,
  onSplitPane,
  onTabDragStart,
  onTabDragEnd,
  onTabDrop,
  onPaneViewModeChange,
  onPaneSourcePathChange,
  onDraftCommit,
  onDraftAttachmentsAdd,
  onDraftAttachmentRemove,
  onComposerError,
  onSend,
  onCancelQueuedPrompt,
  onApprovalDecision,
  onStopSession,
  onKillSession,
  onScrollToBottomRequestHandled,
  onSessionSettingsChange,
}: {
  pane: WorkspacePane;
  codexState: CodexState;
  sessions: Session[];
  isActive: boolean;
  isLoading: boolean;
  draft: string;
  draftAttachments: DraftImageAttachment[];
  isSending: boolean;
  isStopping: boolean;
  isKilling: boolean;
  isUpdating: boolean;
  paneShouldStickToBottomRef: React.MutableRefObject<Record<string, boolean | undefined>>;
  paneScrollPositionsRef: React.MutableRefObject<
    Record<string, Record<string, { top: number; shouldStick: boolean }>>
  >;
  paneContentSignaturesRef: React.MutableRefObject<Record<string, Record<string, string>>>;
  pendingScrollToBottomRequest: {
    sessionId: string;
    token: number;
  } | null;
  draggedTab: {
    sourcePaneId: string;
    sessionId: string;
  } | null;
  onActivatePane: (paneId: string) => void;
  onSelectSession: (paneId: string, sessionId: string) => void;
  onCloseTab: (paneId: string, sessionId: string) => void;
  onSplitPane: (paneId: string, direction: "row" | "column") => void;
  onTabDragStart: (sourcePaneId: string, sessionId: string) => void;
  onTabDragEnd: () => void;
  onTabDrop: (targetPaneId: string, placement: TabDropPlacement, tabIndex?: number) => void;
  onPaneViewModeChange: (paneId: string, viewMode: PaneViewMode) => void;
  onPaneSourcePathChange: (paneId: string, path: string) => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentsAdd: (sessionId: string, attachments: DraftImageAttachment[]) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onComposerError: (message: string | null) => void;
  onSend: (sessionId: string, draftText?: string) => void;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onKillSession: (sessionId: string) => void;
  onScrollToBottomRequestHandled: (token: number) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
}) {
  const activeSession =
    sessions.find((session) => session.id === pane.activeSessionId) ?? sessions[0] ?? null;
  const [sourceDraft, setSourceDraft] = useState(pane.sourcePath ?? "");
  const [fileState, setFileState] = useState<{
    status: "idle" | "loading" | "ready" | "error";
    path: string;
    content: string;
    error: string | null;
    language: string | null;
  }>({
    status: "idle",
    path: "",
    content: "",
    error: null,
    language: null,
  });
  const messageStackRef = useRef<HTMLElement | null>(null);
  const paneTabsRef = useRef<HTMLDivElement | null>(null);
  const [tabRailState, setTabRailState] = useState({
    hasOverflow: false,
    canScrollPrev: false,
    canScrollNext: false,
  });
  const [activeDropPlacement, setActiveDropPlacement] = useState<Exclude<TabDropPlacement, "tabs"> | null>(null);
  const [activeTabInsertIndex, setActiveTabInsertIndex] = useState<number | null>(null);
  const [visitedSessionIds, setVisitedSessionIds] = useState<Record<string, true | undefined>>({});
  const [cachedSessionOrder, setCachedSessionOrder] = useState<string[]>([]);
  const [newResponseIndicatorByKey, setNewResponseIndicatorByKey] = useState<
    Record<string, true | undefined>
  >({});
  const showDropOverlay = Boolean(draggedTab) && !(draggedTab?.sourcePaneId === pane.id && sessions.length <= 1);
  const candidateSourcePaths = useMemo(
    () => (activeSession ? collectCandidateSourcePaths(activeSession) : []),
    [activeSession],
  );
  const commandMessages = useMemo(
    () =>
      activeSession
        ? activeSession.messages.filter((message): message is CommandMessage => message.type === "command")
        : [],
    [activeSession],
  );
  const diffMessages = useMemo(
    () =>
      activeSession
        ? activeSession.messages.filter((message): message is DiffMessage => message.type === "diff")
        : [],
    [activeSession],
  );
  const pendingPrompts = useMemo(() => activeSession?.pendingPrompts ?? [], [activeSession]);
  const mountedSessions = useMemo(() => {
    if (!activeSession) {
      return [];
    }

    const cachedSessionIds = new Set(cachedSessionOrder);
    cachedSessionIds.add(activeSession.id);
    return sessions.filter((session) => cachedSessionIds.has(session.id));
  }, [activeSession, cachedSessionOrder, sessions]);
  const sessionConversationItems = useMemo(
    () => (activeSession ? buildSessionConversationItems(activeSession) : []),
    [activeSession],
  );
  const lastUserPrompt = useMemo(
    () => (activeSession ? findLastUserPrompt(activeSession) : null),
    [activeSession],
  );
  const isSessionBusy =
    activeSession?.status === "active" || activeSession?.status === "approval";
  const showWaitingIndicator =
    pane.viewMode === "session" &&
    Boolean(activeSession) &&
    (activeSession?.status === "active" || (!isSessionBusy && isSending));
  const waitingIndicatorPrompt =
    !isSessionBusy && isSending ? null : lastUserPrompt;
  const composerInputDisabled = !activeSession || isStopping;
  const composerSendDisabled = !activeSession || isSending || isStopping;
  const scrollStateKey =
    pane.viewMode === "source"
      ? `${pane.id}:${pane.viewMode}:${pane.sourcePath ?? "empty"}`
      : `${pane.id}:${pane.viewMode}:${activeSession?.id ?? "empty"}`;
  const defaultScrollToBottom =
    pane.viewMode === "session" || pane.viewMode === "commands" || pane.viewMode === "diffs";
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
        ? buildConversationListSignature(sessionConversationItems)
        : buildMessageListSignature(visibleMessages),
    [pane.viewMode, sessionConversationItems, visibleMessages],
  );
  const visibleLastMessageAuthor = useMemo(
    () =>
      pane.viewMode === "session"
        ? activeSession?.messages[activeSession.messages.length - 1]?.author
        : visibleMessages[visibleMessages.length - 1]?.author,
    [activeSession, pane.viewMode, visibleMessages],
  );
  const showNewResponseIndicator = Boolean(newResponseIndicatorByKey[scrollStateKey]);
  const paneScrollPositions =
    paneScrollPositionsRef.current[pane.id] ?? (paneScrollPositionsRef.current[pane.id] = {});
  const paneContentSignatures =
    paneContentSignaturesRef.current[pane.id] ?? (paneContentSignaturesRef.current[pane.id] = {});

  function getShouldStickToBottom() {
    return paneShouldStickToBottomRef.current[pane.id] ?? true;
  }

  function setShouldStickToBottom(nextValue: boolean) {
    paneShouldStickToBottomRef.current[pane.id] = nextValue;
  }

  function handleComposerPaste(event: ReactClipboardEvent<HTMLTextAreaElement>) {
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
      scrollToLatestMessage("smooth");
      return;
    }

    const node = messageStackRef.current;
    if (!node) {
      return;
    }

    node.scrollTo({
      top: 0,
      behavior: "smooth",
    });
  }

  function handlePaneKeyDown(event: ReactKeyboardEvent<HTMLElement>) {
    if (event.defaultPrevented) {
      return;
    }

    if (event.key !== "PageUp" && event.key !== "PageDown") {
      return;
    }

    if (event.altKey || event.ctrlKey || event.metaKey) {
      return;
    }

    event.preventDefault();
    if (event.shiftKey) {
      scrollMessageStackToBoundary(event.key === "PageUp" ? "top" : "bottom");
    } else {
      scrollMessageStackByPage(event.key === "PageUp" ? -1 : 1);
    }
  }

  function scheduleSettledScrollToBottom(behavior: ScrollBehavior, maxAttempts = 12) {
    let frameId = 0;
    let remainingAttempts = maxAttempts;
    let previousScrollHeight = -1;
    let stableFrameCount = 0;

    const tick = () => {
      scrollToLatestMessage(behavior);

      const node = messageStackRef.current;
      if (!node) {
        return;
      }

      const bottomGap = Math.max(node.scrollHeight - node.clientHeight - node.scrollTop, 0);
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
      }
    };

    frameId = window.requestAnimationFrame(tick);
    return () => {
      window.cancelAnimationFrame(frameId);
    };
  }

  function updateTabRailState() {
    const node = paneTabsRef.current;
    if (!node) {
      setTabRailState((current) =>
        current.hasOverflow || current.canScrollPrev || current.canScrollNext
          ? {
              hasOverflow: false,
              canScrollPrev: false,
              canScrollNext: false,
            }
          : current,
      );
      return;
    }

    const maxScrollLeft = Math.max(node.scrollWidth - node.clientWidth, 0);
    const nextState = {
      hasOverflow: maxScrollLeft > 2,
      canScrollPrev: node.scrollLeft > 2,
      canScrollNext: node.scrollLeft < maxScrollLeft - 2,
    };

    setTabRailState((current) =>
      current.hasOverflow === nextState.hasOverflow &&
      current.canScrollPrev === nextState.canScrollPrev &&
      current.canScrollNext === nextState.canScrollNext
        ? current
        : nextState,
    );
  }

  function scrollTabRail(direction: -1 | 1) {
    const node = paneTabsRef.current;
    if (!node) {
      return;
    }

    const distance = Math.max(Math.round(node.clientWidth * 0.7), 180);
    node.scrollBy({
      left: distance * direction,
      behavior: "smooth",
    });
  }

  function resolveTabInsertIndex(clientX: number) {
    const node = paneTabsRef.current;
    if (!node) {
      return sessions.length;
    }

    const tabNodes = Array.from(node.querySelectorAll<HTMLElement>(".pane-tab-shell"));
    if (tabNodes.length === 0) {
      return 0;
    }

    for (let index = 0; index < tabNodes.length; index += 1) {
      const rect = tabNodes[index].getBoundingClientRect();
      if (clientX < rect.left + rect.width / 2) {
        return index;
      }
    }

    return tabNodes.length;
  }

  function maybeAutoScrollTabRail(clientX: number) {
    const node = paneTabsRef.current;
    if (!node) {
      return;
    }

    const rect = node.getBoundingClientRect();
    const edgeThreshold = Math.min(56, rect.width / 4);
    if (clientX < rect.left + edgeThreshold) {
      node.scrollLeft -= 18;
    } else if (clientX > rect.right - edgeThreshold) {
      node.scrollLeft += 18;
    }
  }

  function handleTabRailDragOver(event: ReactDragEvent<HTMLDivElement>) {
    if (!draggedTab) {
      return;
    }

    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    maybeAutoScrollTabRail(event.clientX);

    const nextTabInsertIndex = resolveTabInsertIndex(event.clientX);
    setActiveDropPlacement(null);
    setActiveTabInsertIndex((current) => (current === nextTabInsertIndex ? current : nextTabInsertIndex));
  }

  function handleTabRailDragLeave(event: ReactDragEvent<HTMLDivElement>) {
    const nextTarget = event.relatedTarget;
    if (nextTarget instanceof Node && event.currentTarget.contains(nextTarget)) {
      return;
    }

    setActiveTabInsertIndex(null);
  }

  function handleTabRailDrop(event: ReactDragEvent<HTMLDivElement>) {
    if (!draggedTab) {
      return;
    }

    event.preventDefault();
    setActiveDropPlacement(null);
    setActiveTabInsertIndex(null);
    onTabDrop(pane.id, "tabs", resolveTabInsertIndex(event.clientX));
  }

  useLayoutEffect(() => {
    const frameId = window.requestAnimationFrame(() => {
      const node = messageStackRef.current;
      if (!node) {
        return;
      }

      const saved = paneScrollPositions[scrollStateKey];
      if (saved) {
        const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
        node.scrollTop = Math.min(saved.top, maxScrollTop);
        setShouldStickToBottom(saved.shouldStick);
        return;
      }

      if (defaultScrollToBottom) {
        node.scrollTop = node.scrollHeight;
        setShouldStickToBottom(true);
        paneScrollPositions[scrollStateKey] = {
          top: node.scrollTop,
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
    if (
      !activeSession ||
      pane.viewMode !== "session" ||
      visitedSessionIds[activeSession.id]
    ) {
      return;
    }

    return scheduleSettledScrollToBottom("auto");
  }, [activeSession, pane.viewMode, scrollStateKey, visitedSessionIds]);

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
      const nextOrder = [activeSession.id, ...current.filter((sessionId) => sessionId !== activeSession.id)].slice(
        0,
        MAX_CACHED_SESSION_PAGES_PER_PANE,
      );

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
    setVisitedSessionIds((current) => pruneSessionFlags(current, availableSessionIds));
    setCachedSessionOrder((current) => {
      const nextOrder = current.filter((sessionId) => availableSessionIds.has(sessionId));
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
    if (!activeSession || pane.viewMode === "source") {
      return;
    }

    const previousSignature = paneContentSignatures[scrollStateKey];
    paneContentSignatures[scrollStateKey] = visibleContentSignature;
    if (previousSignature === undefined || previousSignature === visibleContentSignature) {
      return;
    }

    const shouldScroll =
      getShouldStickToBottom() ||
      paneScrollPositions[scrollStateKey]?.shouldStick === true ||
      visibleLastMessageAuthor === "you";
    if (!shouldScroll) {
      if (pane.viewMode === "session" && visibleLastMessageAuthor === "assistant") {
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
  }, [activeSession, pane.viewMode, scrollStateKey, visibleContentSignature, visibleLastMessageAuthor]);

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
    const cancel = scheduleSettledScrollToBottom("auto");
    const handledFrameId = window.requestAnimationFrame(() => {
      onScrollToBottomRequestHandled(requestToken);
    });

    return () => {
      cancel();
      window.cancelAnimationFrame(handledFrameId);
    };
  }, [
    activeSession?.id,
    isActive,
    onScrollToBottomRequestHandled,
    pane.viewMode,
    pendingScrollToBottomRequest,
    scrollStateKey,
  ]);

  useEffect(() => {
    setSourceDraft(pane.sourcePath ?? "");
  }, [pane.id, pane.sourcePath]);

  useLayoutEffect(() => {
    const frameId = window.requestAnimationFrame(() => {
      const node = paneTabsRef.current;
      if (!node) {
        return;
      }

      const activeTab = node.querySelector<HTMLElement>('.pane-tab-shell[aria-selected="true"]');
      activeTab?.scrollIntoView({
        block: "nearest",
        inline: "nearest",
      });
      updateTabRailState();
    });

    return () => {
      window.cancelAnimationFrame(frameId);
    };
  }, [activeSession?.id, pane.id, sessions.length]);

  useEffect(() => {
    const node = paneTabsRef.current;
    if (!node) {
      return;
    }

    const scheduleUpdate = () => {
      window.requestAnimationFrame(updateTabRailState);
    };

    scheduleUpdate();
    node.addEventListener("scroll", scheduleUpdate, { passive: true });

    const resizeObserver = new ResizeObserver(scheduleUpdate);
    resizeObserver.observe(node);

    return () => {
      node.removeEventListener("scroll", scheduleUpdate);
      resizeObserver.disconnect();
    };
  }, [pane.id, sessions.length]);

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

    if (!pane.sourcePath && candidateSourcePaths[0]) {
      onPaneSourcePathChange(pane.id, candidateSourcePaths[0]);
    }
  }, [candidateSourcePaths, onPaneSourcePathChange, pane.id, pane.sourcePath, pane.viewMode]);

  useEffect(() => {
    let cancelled = false;

    async function loadFile(path: string) {
      setFileState({
        status: "loading",
        path,
        content: "",
        error: null,
        language: null,
      });

      try {
        const response = await fetchFile(path);
        if (cancelled) {
          return;
        }

        setFileState({
          status: "ready",
          path: response.path,
          content: response.content,
          error: null,
          language: response.language ?? null,
        });
      } catch (error) {
        if (cancelled) {
          return;
        }

        setFileState({
          status: "error",
          path,
          content: "",
          error: getErrorMessage(error),
          language: null,
        });
      }
    }

    if (pane.viewMode === "source" && pane.sourcePath) {
      void loadFile(pane.sourcePath);
    } else if (pane.viewMode === "source") {
      setFileState({
        status: "idle",
        path: "",
        content: "",
        error: null,
        language: null,
      });
    }

    return () => {
      cancelled = true;
    };
  }, [pane.sourcePath, pane.viewMode]);

  useEffect(() => {
    if (!showDropOverlay) {
      setActiveDropPlacement(null);
    }
  }, [showDropOverlay]);

  useEffect(() => {
    if (!draggedTab) {
      setActiveTabInsertIndex(null);
    }
  }, [draggedTab]);

  return (
    <section
      className={`workspace-pane thread panel ${isActive ? "active" : ""}`}
      onMouseDown={() => onActivatePane(pane.id)}
      onKeyDown={handlePaneKeyDown}
    >
      {showDropOverlay ? (
        <div className="pane-drop-overlay">
          {(["left", "top", "right", "bottom"] as Exclude<TabDropPlacement, "tabs">[]).map((placement) => (
            <div
              key={placement}
              className={`pane-drop-zone pane-drop-zone-${placement} ${activeDropPlacement === placement ? "active" : ""}`}
              onDragEnter={() => {
                setActiveTabInsertIndex(null);
                setActiveDropPlacement(placement);
              }}
              onDragOver={(event) => {
                event.preventDefault();
                event.dataTransfer.dropEffect = "move";
                setActiveTabInsertIndex(null);
                if (activeDropPlacement !== placement) {
                  setActiveDropPlacement(placement);
                }
              }}
              onDragLeave={() => {
                setActiveDropPlacement((current) => (current === placement ? null : current));
              }}
              onDrop={(event) => {
                event.preventDefault();
                setActiveDropPlacement(null);
                setActiveTabInsertIndex(null);
                onTabDrop(pane.id, placement);
              }}
            >
              <span>{dropLabelForPlacement(placement)}</span>
            </div>
          ))}
        </div>
      ) : null}

      <div className="pane-top">
        <div className="pane-bar">
          <div className="pane-bar-left">
            <div className="pane-tabs-shell">
              {sessions.length > 1 ? (
                <button
                  className={`pane-tab-scroll ${tabRailState.canScrollPrev ? "" : "inactive"}`}
                  type="button"
                  aria-label="Scroll tabs left"
                  onClick={() => scrollTabRail(-1)}
                  aria-disabled={!tabRailState.canScrollPrev}
                >
                  &lt;
                </button>
              ) : null}
              <div
                ref={paneTabsRef}
                className={`pane-tabs ${activeTabInsertIndex === 0 && sessions.length === 0 ? "drop-empty" : ""}`}
                role="tablist"
                aria-label="Tile sessions"
                onDragOver={handleTabRailDragOver}
                onDragLeave={handleTabRailDragLeave}
                onDrop={handleTabRailDrop}
              >
                {sessions.length > 0 ? (
                  sessions.map((session, index) => {
                    const tabActive = session.id === activeSession?.id;
                    const showCodexStatus =
                      session.agent === "Codex" &&
                      Boolean(session.externalSessionId || codexState.rateLimits);
                    const showDropBefore = activeTabInsertIndex === index;
                    const showDropAfter =
                      activeTabInsertIndex === sessions.length && index === sessions.length - 1;
                    const codexStatusTooltipId = showCodexStatus
                      ? `codex-status-${pane.id}-${session.id}`
                      : undefined;

                    return (
                      <div
                        key={session.id}
                        className={`pane-tab-shell ${tabActive ? "active" : ""} ${showCodexStatus ? "has-status-tooltip" : ""} ${showDropBefore ? "drop-before" : ""} ${showDropAfter ? "drop-after" : ""}`}
                        role="tab"
                        aria-selected={tabActive}
                        aria-describedby={codexStatusTooltipId}
                        tabIndex={0}
                        onClick={() => onSelectSession(pane.id, session.id)}
                        onKeyDown={(event) => {
                          if (event.key === "Enter" || event.key === " ") {
                            event.preventDefault();
                            onSelectSession(pane.id, session.id);
                          }
                        }}
                      >
                        <button
                          className="pane-tab-grip"
                          type="button"
                          aria-label={`Drag ${session.name}`}
                          draggable
                          onMouseDown={(event) => {
                            event.stopPropagation();
                          }}
                          onDragStart={(event) => {
                            event.dataTransfer.effectAllowed = "move";
                            event.dataTransfer.setData("text/plain", session.id);
                            const tabShell = event.currentTarget.closest(".pane-tab-shell");
                            if (tabShell instanceof HTMLElement) {
                              const rect = tabShell.getBoundingClientRect();
                              event.dataTransfer.setDragImage(
                                tabShell,
                                Math.max(12, event.clientX - rect.left),
                                Math.max(12, event.clientY - rect.top),
                              );
                            }
                            onTabDragStart(pane.id, session.id);
                          }}
                          onDragEnd={onTabDragEnd}
                        />
                        <span className="pane-tab">
                          <span className="pane-tab-copy">
                            <span className={`status-dot status-${session.status}`} />
                            <span className="pane-tab-label">
                              {session.emoji} {session.name}
                            </span>
                          </span>
                        </span>
                        {showCodexStatus ? (
                          <CodexTabStatusTooltip
                            id={codexStatusTooltipId ?? ""}
                            session={session}
                            codexState={codexState}
                          />
                        ) : null}
                        <button
                          className="pane-tab-close"
                          type="button"
                          draggable={false}
                          aria-label={`Remove ${session.name} from this tile`}
                          onMouseDown={(event) => {
                            event.stopPropagation();
                          }}
                          onClick={(event) => {
                            event.stopPropagation();
                            onCloseTab(pane.id, session.id);
                          }}
                        >
                          ×
                        </button>
                      </div>
                    );
                  })
                ) : (
                  <div className="pane-empty-label">Empty tile</div>
                )}
              </div>
              {sessions.length > 1 ? (
                <button
                  className={`pane-tab-scroll ${tabRailState.canScrollNext ? "" : "inactive"}`}
                  type="button"
                  aria-label="Scroll tabs right"
                  onClick={() => scrollTabRail(1)}
                  aria-disabled={!tabRailState.canScrollNext}
                >
                  &gt;
                </button>
              ) : null}
            </div>
          </div>

        </div>

        <div className="pane-view-strip">
          <div className="pane-view-strip-left">
            {(["session", "prompt", "commands", "diffs", "source"] as PaneViewMode[]).map((viewMode) => (
              <button
                key={viewMode}
                className={`pane-view-button ${pane.viewMode === viewMode ? "selected" : ""}`}
                type="button"
                onClick={() => onPaneViewModeChange(pane.id, viewMode)}
              >
                {labelForPaneViewMode(viewMode)}
              </button>
            ))}
          </div>
          {activeSession ? (
            <div className="thread-chips">
              <span className="chip">{activeSession.workdir}</span>
            </div>
          ) : null}
        </div>
      </div>

      <section
        ref={messageStackRef}
        className="message-stack"
        onScroll={(event) => {
          const node = event.currentTarget;
          const shouldStick = node.scrollHeight - node.scrollTop - node.clientHeight < 72;
          setShouldStickToBottom(shouldStick);
          paneScrollPositions[scrollStateKey] = {
            top: node.scrollTop,
            shouldStick,
          };
          if (shouldStick) {
            setNewResponseIndicator(scrollStateKey, false);
          }
        }}
      >
        <SessionPaneBody
          paneId={pane.id}
          viewMode={pane.viewMode}
          sourcePath={pane.sourcePath}
          scrollContainerRef={messageStackRef}
          activeSession={activeSession}
          isLoading={isLoading}
          isUpdating={isUpdating}
          showWaitingIndicator={showWaitingIndicator}
          waitingIndicatorPrompt={waitingIndicatorPrompt}
          mountedSessions={mountedSessions}
          candidateSourcePaths={candidateSourcePaths}
          fileState={fileState}
          sourceDraft={sourceDraft}
          commandMessages={commandMessages}
          diffMessages={diffMessages}
          onSourceDraftChange={setSourceDraft}
          onPaneSourcePathChange={onPaneSourcePathChange}
          onApprovalDecision={onApprovalDecision}
          onCancelQueuedPrompt={onCancelQueuedPrompt}
          onSessionSettingsChange={onSessionSettingsChange}
        />
      </section>

      {pane.viewMode === "session" ? (
        <SessionComposer
          paneId={pane.id}
          session={activeSession}
          committedDraft={draft}
          draftAttachments={draftAttachments}
          isSending={isSending}
          isStopping={isStopping}
          isSessionBusy={isSessionBusy}
          showNewResponseIndicator={showNewResponseIndicator}
          onScrollToLatest={() => scrollToLatestMessage("smooth")}
          onDraftCommit={onDraftCommit}
          onDraftAttachmentRemove={onDraftAttachmentRemove}
          onComposerError={onComposerError}
          onSend={onSend}
          onStopSession={onStopSession}
          onPaste={handleComposerPaste}
        />
      ) : (
        <footer className="pane-footer-note">
          <p className="composer-hint">
            This tile is in {labelForPaneViewMode(pane.viewMode).toLowerCase()} mode. Use the
            Session tab to send prompts.
          </p>
        </footer>
      )}
    </section>
  );
}

const SessionPaneBody = memo(function SessionPaneBody({
  paneId,
  viewMode,
  sourcePath,
  scrollContainerRef,
  activeSession,
  isLoading,
  isUpdating,
  showWaitingIndicator,
  waitingIndicatorPrompt,
  mountedSessions,
  candidateSourcePaths,
  fileState,
  sourceDraft,
  commandMessages,
  diffMessages,
  onSourceDraftChange,
  onPaneSourcePathChange,
  onApprovalDecision,
  onCancelQueuedPrompt,
  onSessionSettingsChange,
}: {
  paneId: string;
  viewMode: PaneViewMode;
  sourcePath: string | null;
  scrollContainerRef: RefObject<HTMLElement | null>;
  activeSession: Session | null;
  isLoading: boolean;
  isUpdating: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorPrompt: string | null;
  mountedSessions: Session[];
  candidateSourcePaths: string[];
  fileState: {
    status: "idle" | "loading" | "ready" | "error";
    path: string;
    content: string;
    error: string | null;
    language: string | null;
  };
  sourceDraft: string;
  commandMessages: CommandMessage[];
  diffMessages: DiffMessage[];
  onSourceDraftChange: (nextValue: string) => void;
  onPaneSourcePathChange: (paneId: string, path: string) => void;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
}) {
  if (!activeSession) {
    return (
      <EmptyState
        title="Ready for a session"
        body="Click a session on the left to open it in the active tile."
      />
    );
  }

  if (viewMode === "session") {
    const activePendingPrompts = activeSession.pendingPrompts ?? [];
    if (activeSession.messages.length === 0 && activePendingPrompts.length === 0 && !showWaitingIndicator) {
      return (
        <EmptyState
          title={isLoading ? "Connecting to backend" : "Live session is ready"}
          body={
            isLoading
              ? "Fetching session state from the Rust backend."
              : `Send a prompt to ${activeSession.agent} and this tile will fill with live cards.`
          }
        />
      );
    }

    return (
      <>
        {mountedSessions.map((session) => (
          <SessionConversationPage
            key={session.id}
            session={session}
            scrollContainerRef={scrollContainerRef}
            isActive={session.id === activeSession.id}
            isLoading={isLoading && session.id === activeSession.id}
            showWaitingIndicator={showWaitingIndicator && session.id === activeSession.id}
            waitingIndicatorPrompt={session.id === activeSession.id ? waitingIndicatorPrompt : null}
            onApprovalDecision={onApprovalDecision}
            onCancelQueuedPrompt={onCancelQueuedPrompt}
          />
        ))}
      </>
    );
  }

  if (viewMode === "prompt") {
    if (activeSession.agent === "Codex") {
      return (
        <CodexPromptSettingsCard
          paneId={paneId}
          session={activeSession}
          isUpdating={isUpdating}
          onSessionSettingsChange={onSessionSettingsChange}
        />
      );
    }

    if (activeSession.agent === "Claude") {
      return (
        <ClaudePromptSettingsCard
          paneId={paneId}
          session={activeSession}
          isUpdating={isUpdating}
          onSessionSettingsChange={onSessionSettingsChange}
        />
      );
    }

    return (
      <EmptyState
        title="No prompt settings"
        body="Prompt controls are only available for supported agent sessions."
      />
    );
  }

  if (viewMode === "commands") {
    return commandMessages.length > 0 ? (
      <>
        {commandMessages.map((message) => (
          <CommandCard key={message.id} message={message} />
        ))}
      </>
    ) : (
      <EmptyState
        title="No commands yet"
        body="This tile is filtered to command executions. Send a prompt that runs tools and they will show up here."
      />
    );
  }

  if (viewMode === "diffs") {
    return diffMessages.length > 0 ? (
      <>
        {diffMessages.map((message) => (
          <DiffCard key={message.id} message={message} />
        ))}
      </>
    ) : (
      <EmptyState
        title="No diffs yet"
        body="This tile is filtered to file changes. When the agent edits or creates files, the diffs will appear here."
      />
    );
  }

  return (
    <SourcePane
      candidatePaths={candidateSourcePaths}
      fileState={fileState}
      sourceDraft={sourceDraft}
      sourcePath={sourcePath}
      onDraftChange={onSourceDraftChange}
      onOpenPath={(path) => onPaneSourcePathChange(paneId, path)}
    />
  );
}, (previous, next) =>
  previous.paneId === next.paneId &&
  previous.viewMode === next.viewMode &&
  previous.sourcePath === next.sourcePath &&
  previous.scrollContainerRef === next.scrollContainerRef &&
  previous.activeSession === next.activeSession &&
  previous.isLoading === next.isLoading &&
  previous.isUpdating === next.isUpdating &&
  previous.showWaitingIndicator === next.showWaitingIndicator &&
  previous.waitingIndicatorPrompt === next.waitingIndicatorPrompt &&
  previous.mountedSessions === next.mountedSessions &&
  previous.candidateSourcePaths === next.candidateSourcePaths &&
  previous.fileState === next.fileState &&
  previous.sourceDraft === next.sourceDraft &&
  previous.commandMessages === next.commandMessages &&
  previous.diffMessages === next.diffMessages
);

const SessionConversationPage = memo(function SessionConversationPage({
  session,
  scrollContainerRef,
  isActive,
  isLoading,
  showWaitingIndicator,
  waitingIndicatorPrompt,
  onApprovalDecision,
  onCancelQueuedPrompt,
}: {
  session: Session;
  scrollContainerRef: RefObject<HTMLElement | null>;
  isActive: boolean;
  isLoading: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorPrompt: string | null;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
}) {
  const pendingPrompts = session.pendingPrompts ?? [];

  if (session.messages.length === 0 && pendingPrompts.length === 0 && !showWaitingIndicator) {
    return (
      <div
        className={`session-conversation-page${isActive ? " is-active" : ""}`}
        hidden={!isActive}
      >
        <EmptyState
          title={isLoading ? "Connecting to backend" : "Live session is ready"}
          body={
            isLoading
              ? "Fetching session state from the Rust backend."
              : `Send a prompt to ${session.agent} and this tile will fill with live cards.`
          }
        />
      </div>
    );
  }

  return (
    <div
      className={`session-conversation-page${isActive ? " is-active" : ""}`}
      hidden={!isActive}
    >
      <ConversationMessageList
        sessionId={session.id}
        messages={session.messages}
        scrollContainerRef={scrollContainerRef}
        isActive={isActive}
        onApprovalDecision={onApprovalDecision}
      />

      {showWaitingIndicator ? (
        <RunningIndicator agent={session.agent} lastPrompt={waitingIndicatorPrompt} />
      ) : null}

      {pendingPrompts.map((prompt) => (
        <PendingPromptCard
          key={prompt.id}
          prompt={prompt}
          onCancel={() => onCancelQueuedPrompt(session.id, prompt.id)}
        />
      ))}
    </div>
  );
}, (previous, next) =>
  previous.session === next.session &&
  previous.scrollContainerRef === next.scrollContainerRef &&
  previous.isActive === next.isActive &&
  previous.isLoading === next.isLoading &&
  previous.showWaitingIndicator === next.showWaitingIndicator &&
  previous.waitingIndicatorPrompt === next.waitingIndicatorPrompt
);

function ConversationMessageList({
  sessionId,
  messages,
  scrollContainerRef,
  isActive,
  onApprovalDecision,
}: {
  sessionId: string;
  messages: Message[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  isActive: boolean;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
}) {
  if (!isActive || messages.length < CONVERSATION_VIRTUALIZATION_MIN_MESSAGES) {
    return (
      <>
        {messages.map((message, index) => (
          <MessageCard
            key={message.id}
            message={message}
            preferImmediateHeavyRender={isActive && index >= messages.length - 2}
            onApprovalDecision={(messageId, decision) => onApprovalDecision(sessionId, messageId, decision)}
          />
        ))}
      </>
    );
  }

  return (
    <VirtualizedConversationMessageList
      sessionId={sessionId}
      messages={messages}
      scrollContainerRef={scrollContainerRef}
      onApprovalDecision={onApprovalDecision}
    />
  );
}

function VirtualizedConversationMessageList({
  sessionId,
  messages,
  scrollContainerRef,
  onApprovalDecision,
}: {
  sessionId: string;
  messages: Message[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
}) {
  const messageHeightsRef = useRef<Record<string, number>>({});
  const visibleRangeRef = useRef({
    startIndex: 0,
    endIndex: messages.length,
  });
  const [viewport, setViewport] = useState({
    height: DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
    scrollTop: 0,
  });
  const [layoutVersion, setLayoutVersion] = useState(0);

  const messageIndexById = useMemo(
    () => new Map(messages.map((message, index) => [message.id, index])),
    [messages],
  );
  const messageHeights = useMemo(
    () =>
      messages.map(
        (message) =>
          messageHeightsRef.current[message.id] ?? estimateConversationMessageHeight(message),
      ),
    [layoutVersion, messages],
  );
  const layout = useMemo(
    () => buildVirtualizedMessageLayout(messageHeights),
    [messageHeights],
  );
  const activeViewport = scrollContainerRef.current;
  const viewportHeight =
    activeViewport?.clientHeight && activeViewport.clientHeight > 0
      ? activeViewport.clientHeight
      : viewport.height;
  const viewportScrollTop = activeViewport ? activeViewport.scrollTop : viewport.scrollTop;
  const visibleRange = useMemo(
    () =>
      findVirtualizedMessageRange(
        layout.tops,
        messageHeights,
        viewportScrollTop,
        viewportHeight,
        VIRTUALIZED_MESSAGE_OVERSCAN_PX,
      ),
    [layout.tops, messageHeights, viewportHeight, viewportScrollTop],
  );

  useEffect(() => {
    visibleRangeRef.current = visibleRange;
  }, [visibleRange]);

  useEffect(() => {
    messageHeightsRef.current = Object.fromEntries(
      messages
        .filter((message) => messageHeightsRef.current[message.id] !== undefined)
        .map((message) => [message.id, messageHeightsRef.current[message.id] as number]),
    );
  }, [messages]);

  useLayoutEffect(() => {
    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    const syncViewport = () => {
      const nextState = {
        height: node.clientHeight > 0 ? node.clientHeight : DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
        scrollTop: node.scrollTop,
      };

      setViewport((current) =>
        current.height === nextState.height && current.scrollTop === nextState.scrollTop
          ? current
          : nextState,
      );
    };

    syncViewport();
    node.addEventListener("scroll", syncViewport, { passive: true });
    const resizeObserver = new ResizeObserver(syncViewport);
    resizeObserver.observe(node);

    return () => {
      node.removeEventListener("scroll", syncViewport);
      resizeObserver.disconnect();
    };
  }, [scrollContainerRef, sessionId]);

  function handleHeightChange(messageId: string, nextHeight: number) {
    if (!Number.isFinite(nextHeight) || nextHeight <= 0) {
      return;
    }

    const previousHeight =
      messageHeightsRef.current[messageId] ?? estimateConversationMessageHeight(messages[messageIndexById.get(messageId) ?? 0]);
    if (Math.abs(previousHeight - nextHeight) < 1) {
      return;
    }

    messageHeightsRef.current[messageId] = nextHeight;

    const messageIndex = messageIndexById.get(messageId);
    const node = scrollContainerRef.current;
    if (
      node &&
      messageIndex !== undefined &&
      messageIndex < visibleRangeRef.current.startIndex
    ) {
      node.scrollTop += nextHeight - previousHeight;
    }

    setLayoutVersion((current) => current + 1);
  }

  return (
    <div className="virtualized-message-list" style={{ height: layout.totalHeight }}>
      {messages
        .slice(visibleRange.startIndex, visibleRange.endIndex)
        .map((message, visibleIndex) => {
          const messageIndex = visibleRange.startIndex + visibleIndex;
          return (
            <MeasuredMessageCard
              key={message.id}
              message={message}
              preferImmediateHeavyRender={messageIndex >= messages.length - 2}
              top={layout.tops[messageIndex] ?? 0}
              onApprovalDecision={(messageId, decision) => onApprovalDecision(sessionId, messageId, decision)}
              onHeightChange={handleHeightChange}
            />
          );
        })}
    </div>
  );
}

function MeasuredMessageCard({
  message,
  preferImmediateHeavyRender,
  onApprovalDecision,
  onHeightChange,
  top,
}: {
  message: Message;
  preferImmediateHeavyRender: boolean;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  onHeightChange: (messageId: string, nextHeight: number) => void;
  top: number;
}) {
  const slotRef = useRef<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    const node = slotRef.current;
    if (!node) {
      return;
    }

    let frameId = 0;
    const measure = () => {
      frameId = 0;
      onHeightChange(message.id, node.getBoundingClientRect().height);
    };

    measure();
    const resizeObserver = new ResizeObserver(() => {
      if (frameId !== 0) {
        return;
      }

      frameId = window.requestAnimationFrame(measure);
    });
    resizeObserver.observe(node);

    return () => {
      resizeObserver.disconnect();
      if (frameId !== 0) {
        window.cancelAnimationFrame(frameId);
      }
    };
  }, [message, onHeightChange]);

  return (
    <div ref={slotRef} className="virtualized-message-slot" style={{ top }}>
      <MessageCard
        message={message}
        preferImmediateHeavyRender={preferImmediateHeavyRender}
        onApprovalDecision={onApprovalDecision}
      />
    </div>
  );
}

const SessionComposer = memo(function SessionComposer({
  paneId,
  session,
  committedDraft,
  draftAttachments,
  isSending,
  isStopping,
  isSessionBusy,
  showNewResponseIndicator,
  onScrollToLatest,
  onDraftCommit,
  onDraftAttachmentRemove,
  onComposerError,
  onSend,
  onStopSession,
  onPaste,
}: {
  paneId: string;
  session: Session | null;
  committedDraft: string;
  draftAttachments: DraftImageAttachment[];
  isSending: boolean;
  isStopping: boolean;
  isSessionBusy: boolean;
  showNewResponseIndicator: boolean;
  onScrollToLatest: () => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onComposerError: (message: string | null) => void;
  onSend: (sessionId: string, draftText?: string) => void;
  onStopSession: (sessionId: string) => void;
  onPaste: (event: ReactClipboardEvent<HTMLTextAreaElement>) => void;
}) {
  const composerInputRef = useRef<HTMLTextAreaElement | null>(null);
  const localDraftsRef = useRef<Record<string, string>>({});
  const committedDraftsRef = useRef<Record<string, string>>({});
  const [localDraftsBySessionId, setLocalDraftsBySessionId] = useState<Record<string, string>>({});
  const [promptHistoryStateBySessionId, setPromptHistoryStateBySessionId] = useState<
    Record<string, PromptHistoryState | undefined>
  >({});

  const activeSessionId = session?.id ?? null;
  const composerDraft =
    activeSessionId === null
      ? ""
      : (localDraftsBySessionId[activeSessionId] ?? committedDraft);
  const composerInputDisabled = !session || isStopping;
  const composerSendDisabled = !session || isSending || isStopping;

  function resizeComposerInput() {
    const textarea = composerInputRef.current;
    if (!textarea) {
      return;
    }

    const computedStyle = window.getComputedStyle(textarea);
    const minHeight = parseFloat(computedStyle.minHeight) || 0;
    const borderHeight =
      (parseFloat(computedStyle.borderTopWidth) || 0) + (parseFloat(computedStyle.borderBottomWidth) || 0);
    const panelElement = textarea.closest(".workspace-pane");
    const panelSlotElement =
      panelElement instanceof HTMLElement && panelElement.parentElement instanceof HTMLElement
        ? panelElement.parentElement
        : null;
    const availablePanelHeight =
      panelSlotElement?.clientHeight ?? (panelElement instanceof HTMLElement ? panelElement.clientHeight : 0);
    const maxHeight = Math.max(
      minHeight,
      availablePanelHeight > 0 ? availablePanelHeight * 0.4 : Number.POSITIVE_INFINITY,
    );

    textarea.style.height = "0px";

    const contentHeight = textarea.scrollHeight + borderHeight;
    const nextHeight = Math.min(Math.max(contentHeight, minHeight), maxHeight);
    textarea.style.height = `${nextHeight}px`;
    textarea.style.overflowY = contentHeight > maxHeight + 1 ? "auto" : "hidden";
  }

  useLayoutEffect(() => {
    resizeComposerInput();
  }, [activeSessionId, composerDraft]);

  useEffect(() => {
    localDraftsRef.current = localDraftsBySessionId;
  }, [localDraftsBySessionId]);

  useEffect(() => {
    const textarea = composerInputRef.current;
    if (!textarea || typeof ResizeObserver === "undefined") {
      return;
    }

    const panelElement = textarea.closest(".workspace-pane");
    const panelSlotElement =
      panelElement instanceof HTMLElement && panelElement.parentElement instanceof HTMLElement
        ? panelElement.parentElement
        : null;
    let previousWidth = textarea.getBoundingClientRect().width;
    let previousAvailablePanelHeight =
      panelSlotElement?.clientHeight ?? (panelElement instanceof HTMLElement ? panelElement.clientHeight : 0);
    const resizeObserver = new ResizeObserver((entries) => {
      const nextWidth =
        entries.find((entry) => entry.target === textarea)?.contentRect.width ??
        textarea.getBoundingClientRect().width;
      const nextAvailablePanelHeight =
        panelSlotElement?.clientHeight ?? (panelElement instanceof HTMLElement ? panelElement.clientHeight : 0);
      const widthChanged = Math.abs(nextWidth - previousWidth) >= 1;
      const panelHeightChanged =
        Math.abs(nextAvailablePanelHeight - previousAvailablePanelHeight) >= 1;

      if (!widthChanged && !panelHeightChanged) {
        return;
      }

      previousWidth = nextWidth;
      previousAvailablePanelHeight = nextAvailablePanelHeight;
      resizeComposerInput();
    });

    resizeObserver.observe(textarea);
    if (panelSlotElement instanceof HTMLElement) {
      resizeObserver.observe(panelSlotElement);
    } else if (panelElement instanceof HTMLElement) {
      resizeObserver.observe(panelElement);
    }

    return () => {
      resizeObserver.disconnect();
    };
  }, [activeSessionId]);

  useEffect(() => {
    if (!activeSessionId) {
      return;
    }

    const previousCommitted = committedDraftsRef.current[activeSessionId];
    const localDraft = localDraftsRef.current[activeSessionId];

    committedDraftsRef.current[activeSessionId] = committedDraft;

    if (localDraft !== undefined && localDraft !== previousCommitted) {
      return;
    }

    setLocalDraftsBySessionId((current) => {
      if ((current[activeSessionId] ?? "") === committedDraft) {
        return current;
      }

      return {
        ...current,
        [activeSessionId]: committedDraft,
      };
    });
  }, [activeSessionId, committedDraft]);

  useEffect(() => {
    if (!activeSessionId) {
      return;
    }

    return () => {
      const latestDraft = localDraftsRef.current[activeSessionId];
      const committed = committedDraftsRef.current[activeSessionId] ?? "";
      if (latestDraft !== undefined && latestDraft !== committed) {
        committedDraftsRef.current[activeSessionId] = latestDraft;
        onDraftCommit(activeSessionId, latestDraft);
      }
    };
  }, [activeSessionId, onDraftCommit]);

  function resetPromptHistory(sessionId: string) {
    setPromptHistoryStateBySessionId((current) => {
      if (!current[sessionId]) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[sessionId];
      return nextState;
    });
  }

  function updateLocalDraft(sessionId: string, nextValue: string) {
    localDraftsRef.current = {
      ...localDraftsRef.current,
      [sessionId]: nextValue,
    };

    setLocalDraftsBySessionId((current) => {
      if ((current[sessionId] ?? "") === nextValue) {
        return current;
      }

      return {
        ...current,
        [sessionId]: nextValue,
      };
    });
  }

  function commitDraft(sessionId: string, nextValue: string) {
    committedDraftsRef.current[sessionId] = nextValue;
    onDraftCommit(sessionId, nextValue);
  }

  function handleComposerChange(nextValue: string) {
    if (!activeSessionId) {
      return;
    }

    resetPromptHistory(activeSessionId);
    updateLocalDraft(activeSessionId, nextValue);
  }

  function handleComposerBlur() {
    if (!activeSessionId) {
      return;
    }

    commitDraft(activeSessionId, composerDraft);
  }

  function handleComposerSend() {
    if (!session) {
      return;
    }

    const draftToSend = composerDraft;
    resetPromptHistory(session.id);
    updateLocalDraft(session.id, "");
    commitDraft(session.id, "");
    onSend(session.id, draftToSend);
    window.requestAnimationFrame(() => {
      composerInputRef.current?.focus();
    });
  }

  function handleComposerKeyDown(event: ReactKeyboardEvent<HTMLTextAreaElement>) {
    if (!session) {
      return;
    }

    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      handleComposerSend();
      return;
    }

    if (event.key !== "ArrowUp" && event.key !== "ArrowDown") {
      return;
    }

    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) {
      return;
    }

    const textarea = event.currentTarget;
    if (textarea.selectionStart !== 0 || textarea.selectionEnd !== 0) {
      return;
    }

    const promptHistory = collectUserPromptHistory(session);
    if (promptHistory.length === 0) {
      return;
    }

    const historyState = promptHistoryStateBySessionId[session.id];
    if (event.key === "ArrowDown" && !historyState) {
      return;
    }

    event.preventDefault();

    if (event.key === "ArrowUp") {
      const nextIndex = historyState ? Math.max(historyState.index - 1, 0) : promptHistory.length - 1;
      const draftSnapshot = historyState?.draft ?? composerDraft;

      setPromptHistoryStateBySessionId((current) => ({
        ...current,
        [session.id]: {
          index: nextIndex,
          draft: draftSnapshot,
        },
      }));
      updateLocalDraft(session.id, promptHistory[nextIndex]);
    } else {
      const currentHistoryState = historyState;
      if (!currentHistoryState) {
        return;
      }

      if (currentHistoryState.index >= promptHistory.length - 1) {
        resetPromptHistory(session.id);
        updateLocalDraft(session.id, currentHistoryState.draft);
      } else {
        const nextIndex = currentHistoryState.index + 1;
        setPromptHistoryStateBySessionId((current) => ({
          ...current,
          [session.id]: {
            index: nextIndex,
            draft: currentHistoryState.draft,
          },
        }));
        updateLocalDraft(session.id, promptHistory[nextIndex]);
      }
    }

    window.requestAnimationFrame(() => {
      textarea.setSelectionRange(0, 0);
    });
  }

  return (
    <footer className="composer">
      {showNewResponseIndicator ? (
        <button className="new-response-indicator" type="button" onClick={onScrollToLatest}>
          New response
        </button>
      ) : null}
      {draftAttachments.length > 0 ? (
        <div className="composer-attachments" aria-label="Draft image attachments">
          {draftAttachments.map((attachment) => (
            <article key={attachment.id} className="composer-attachment-card">
              <img
                className="composer-attachment-preview"
                src={attachment.previewUrl}
                alt={attachment.fileName}
              />
              <div className="composer-attachment-copy">
                <strong className="composer-attachment-name">{attachment.fileName}</strong>
                <span className="composer-attachment-meta">
                  {formatByteSize(attachment.byteSize)} · {attachment.mediaType}
                </span>
              </div>
              <button
                className="composer-attachment-remove"
                type="button"
                onClick={() => session && onDraftAttachmentRemove(session.id, attachment.id)}
                aria-label={`Remove ${attachment.fileName}`}
                disabled={composerInputDisabled}
              >
                Remove
              </button>
            </article>
          ))}
        </div>
      ) : null}
      <div className="composer-row">
        <textarea
          id={`prompt-${paneId}`}
          ref={composerInputRef}
          className="composer-input"
          aria-label={session ? `Message ${session.name}` : "Message session"}
          value={composerDraft}
          onChange={(event) => handleComposerChange(event.target.value)}
          onBlur={handleComposerBlur}
          disabled={composerInputDisabled}
          onKeyDown={handleComposerKeyDown}
          onPaste={onPaste}
          placeholder={session ? `Send a prompt to ${session.agent}...` : "Open a session..."}
          rows={1}
        />
        <div className="composer-actions">
          {session && (isSessionBusy || isStopping) ? (
            <button
              className="ghost-button composer-stop-button"
              type="button"
              onClick={() => onStopSession(session.id)}
              disabled={isStopping}
            >
              {isStopping ? "Stopping..." : "Stop"}
            </button>
          ) : null}
          <button
            className="send-button"
            type="button"
            onMouseDown={(event) => {
              event.preventDefault();
            }}
            onClick={handleComposerSend}
            disabled={composerSendDisabled}
          >
            {isSending ? (isSessionBusy ? "Queueing..." : "Sending...") : isSessionBusy ? "Queue" : "Send"}
          </button>
        </div>
      </div>
    </footer>
  );
}, (previous, next) =>
  previous.paneId === next.paneId &&
  previous.session === next.session &&
  previous.committedDraft === next.committedDraft &&
  previous.draftAttachments === next.draftAttachments &&
  previous.isSending === next.isSending &&
  previous.isStopping === next.isStopping &&
  previous.isSessionBusy === next.isSessionBusy &&
  previous.showNewResponseIndicator === next.showNewResponseIndicator
);

function EmptyState({ title, body }: { title: string; body: string }) {
  return (
    <article className="empty-state">
      <div className="card-label">Live State</div>
      <h3>{title}</h3>
      <p>{body}</p>
    </article>
  );
}

function CodexPromptSettingsCard({
  paneId,
  session,
  isUpdating,
  onSessionSettingsChange,
}: {
  paneId: string;
  session: Session;
  isUpdating: boolean;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
}) {
  return (
    <article className="message-card prompt-settings-card">
      <div className="card-label">Next Prompt</div>
      <h3>Prompt permissions</h3>
      <div className="prompt-settings-grid">
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`sandbox-mode-${paneId}`}>
            Next prompt sandbox
          </label>
          <ThemedCombobox
            id={`sandbox-mode-${paneId}`}
            className="prompt-settings-select"
            value={session.sandboxMode ?? "workspace-write"}
            options={SANDBOX_MODE_OPTIONS as readonly ComboboxOption[]}
            disabled={isUpdating}
            onChange={(nextValue) =>
              void onSessionSettingsChange(session.id, "sandboxMode", nextValue as SandboxMode)
            }
          />
        </div>
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`approval-policy-${paneId}`}>
            Next prompt approval
          </label>
          <ThemedCombobox
            id={`approval-policy-${paneId}`}
            className="prompt-settings-select"
            value={session.approvalPolicy ?? "never"}
            options={APPROVAL_POLICY_OPTIONS as readonly ComboboxOption[]}
            disabled={isUpdating}
            onChange={(nextValue) =>
              void onSessionSettingsChange(
                session.id,
                "approvalPolicy",
                nextValue as ApprovalPolicy,
              )
            }
          />
        </div>
        <p className="session-control-hint">
          Changes apply on the next Codex prompt. If the underlying thread was started with a
          different sandbox, TermAl starts a fresh Codex thread for the next turn.
        </p>
      </div>
    </article>
  );
}

function ClaudePromptSettingsCard({
  paneId,
  session,
  isUpdating,
  onSessionSettingsChange,
}: {
  paneId: string;
  session: Session;
  isUpdating: boolean;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
}) {
  return (
    <article className="message-card prompt-settings-card">
      <div className="card-label">Session Mode</div>
      <h3>Claude approvals</h3>
      <div className="prompt-settings-grid">
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`claude-approval-mode-${paneId}`}>
            Claude approval mode
          </label>
          <ThemedCombobox
            id={`claude-approval-mode-${paneId}`}
            className="prompt-settings-select"
            value={session.claudeApprovalMode ?? "ask"}
            options={CLAUDE_APPROVAL_OPTIONS as readonly ComboboxOption[]}
            disabled={isUpdating}
            onChange={(nextValue) =>
              void onSessionSettingsChange(
                session.id,
                "claudeApprovalMode",
                nextValue as ClaudeApprovalMode,
              )
            }
          />
        </div>
        <p className="session-control-hint">
          Ask keeps the current approval cards. Auto-approve lets Claude continue without pausing
          when it requests tool permission.
        </p>
      </div>
    </article>
  );
}

function RunningIndicator({
  agent,
  lastPrompt,
}: {
  agent: Session["agent"];
  lastPrompt: string | null;
}) {
  const tooltipId = useId();

  return (
    <article
      className={`activity-card activity-card-live ${lastPrompt ? "has-tooltip" : ""}`}
      role="status"
      aria-live="polite"
      aria-describedby={lastPrompt ? tooltipId : undefined}
      tabIndex={lastPrompt ? 0 : undefined}
    >
      <div className="activity-spinner" aria-hidden="true" />
      <div>
        <div className="card-label">Live turn</div>
        <h3>{agent} is working</h3>
        <p>Waiting for the next chunk of output...</p>
      </div>
      {lastPrompt ? (
        <div id={tooltipId} className="activity-tooltip" role="tooltip">
          <div className="activity-tooltip-label">Last prompt</div>
          <p>{lastPrompt}</p>
        </div>
      ) : null}
    </article>
  );
}

function CodexTabStatusTooltip({
  codexState,
  id,
  session,
}: {
  codexState: CodexState;
  id: string;
  session: Session;
}) {
  const rateLimits = codexState.rateLimits;

  return (
    <div id={id} className="pane-tab-status-tooltip" role="tooltip">
      <div className="pane-tab-status-header">
        <div className="activity-tooltip-label">Status</div>
        {rateLimits?.planType ? (
          <span className="pane-tab-status-plan">{rateLimits.planType}</span>
        ) : null}
      </div>
      <div className="pane-tab-status-grid">
        {session.externalSessionId ? (
          <>
            <div className="pane-tab-status-key">Session:</div>
            <div className="pane-tab-status-value pane-tab-status-mono">
              {session.externalSessionId}
            </div>
          </>
        ) : null}
        {rateLimits?.primary ? (
          <>
            <div className="pane-tab-status-key">5h limit:</div>
            <div className="pane-tab-status-value">
              <CodexRateLimitMeter label="5h limit" window={rateLimits.primary} />
            </div>
          </>
        ) : null}
        {rateLimits?.secondary ? (
          <>
            <div className="pane-tab-status-key">7d limit:</div>
            <div className="pane-tab-status-value">
              <CodexRateLimitMeter label="7d limit" window={rateLimits.secondary} />
            </div>
          </>
        ) : null}
      </div>
    </div>
  );
}

function CodexRateLimitMeter({
  label,
  window,
}: {
  label: string;
  window: CodexRateLimitWindow;
}) {
  const usedPercent = clamp(Math.round(window.usedPercent ?? 0), 0, 100);
  const remainingPercent = clamp(100 - usedPercent, 0, 100);
  const resetsLabel = formatRateLimitResetLabel(window.resetsAt ?? null, label);

  return (
    <div className="codex-limit-row">
      <div className="codex-limit-bar" aria-hidden="true">
        <div className="codex-limit-bar-fill" style={{ width: `${remainingPercent}%` }} />
        <div className="codex-limit-bar-used" style={{ width: `${usedPercent}%` }} />
      </div>
      <div className="codex-limit-meta">
        <strong>{remainingPercent}% left</strong>
        {resetsLabel ? <span>({resetsLabel})</span> : null}
      </div>
    </div>
  );
}

function SourcePane({
  candidatePaths,
  fileState,
  sourceDraft,
  sourcePath,
  onDraftChange,
  onOpenPath,
}: {
  candidatePaths: string[];
  fileState: {
    status: "idle" | "loading" | "ready" | "error";
    path: string;
    content: string;
    error: string | null;
    language: string | null;
  };
  sourceDraft: string;
  sourcePath: string | null;
  onDraftChange: (nextValue: string) => void;
  onOpenPath: (path: string) => void;
}) {
  return (
    <div className="source-pane">
      <div className="source-toolbar">
        <div className="source-path-row">
          <input
            className="source-path-input"
            type="text"
            value={sourceDraft}
            onChange={(event) => onDraftChange(event.target.value)}
            placeholder="/absolute/path/to/file.rs"
          />
          <button
            className="ghost-button"
            type="button"
            onClick={() => onOpenPath(sourceDraft.trim())}
            disabled={!sourceDraft.trim()}
          >
            Open
          </button>
        </div>

        {candidatePaths.length > 0 ? (
          <div className="source-chip-row">
            {candidatePaths.map((path) => (
              <button
                key={path}
                className={`chip source-chip ${path === sourcePath ? "selected" : ""}`}
                type="button"
                onClick={() => onOpenPath(path)}
              >
                {path}
              </button>
            ))}
          </div>
        ) : null}
      </div>

      {fileState.status === "idle" ? (
        <EmptyState
          title="No source file selected"
          body="Pick a touched file above or enter a path manually to open the source in this tile."
        />
      ) : null}

      {fileState.status === "loading" ? (
        <article className="activity-card">
          <div className="activity-spinner" aria-hidden="true" />
          <div>
            <div className="card-label">Source</div>
            <h3>Loading file</h3>
            <p>{fileState.path}</p>
          </div>
        </article>
      ) : null}

      {fileState.status === "error" ? (
        <article className="thread-notice">
          <div className="card-label">Source</div>
          <p>{fileState.error}</p>
        </article>
      ) : null}

      {fileState.status === "ready" ? (
        <article className="message-card source-file-card">
          <div className="message-meta">
            <span>Source</span>
            <span>{fileState.path}</span>
          </div>
          <DeferredHighlightedCodeBlock
            className="code-block source-code-block"
            code={fileState.content}
            language={fileState.language}
            pathHint={fileState.path}
          />
        </article>
      ) : null}
    </div>
  );
}

const MessageCard = memo(function MessageCard({
  message,
  preferImmediateHeavyRender = false,
  onApprovalDecision,
}: {
  message: Message;
  preferImmediateHeavyRender?: boolean;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
}) {
  switch (message.type) {
    case "text": {
      const connectionRetryNotice =
        message.author === "assistant" ? parseConnectionRetryNotice(message.text) : null;

      if (connectionRetryNotice) {
        return <ConnectionRetryCard message={message} notice={connectionRetryNotice} />;
      }

      return (
        <article className={`message-card bubble bubble-${message.author}`}>
          <MessageMeta author={message.author} timestamp={message.timestamp} />
          {message.attachments && message.attachments.length > 0 ? (
            <MessageAttachmentList attachments={message.attachments} />
          ) : null}
          {message.author === "assistant" ? (
            preferImmediateHeavyRender ? (
              <MarkdownContent markdown={message.text} />
            ) : (
              <DeferredMarkdownContent markdown={message.text} />
            )
          ) : message.text ? (
            <p className="plain-text-copy">{message.text}</p>
          ) : (
            <p className="support-copy">{imageAttachmentSummaryLabel(message.attachments?.length ?? 0)}</p>
          )}
        </article>
      );
    }
    case "thinking":
      return <ThinkingCard message={message} />;
    case "command":
      return <CommandCard message={message} />;
    case "diff":
      return <DiffCard message={message} />;
    case "markdown":
      return <MarkdownCard message={message} />;
    case "approval":
      return <ApprovalCard message={message} onApprovalDecision={onApprovalDecision} />;
    default:
      return null;
  }
}, (previous, next) =>
  previous.message === next.message &&
  previous.preferImmediateHeavyRender === next.preferImmediateHeavyRender
);

const PendingPromptCard = memo(function PendingPromptCard({
  prompt,
  onCancel,
}: {
  prompt: PendingPrompt;
  onCancel: () => void;
}) {
  return (
    <article className="message-card bubble bubble-you pending-prompt-card">
      <div className="pending-prompt-header">
        <MessageMeta author="you" timestamp={prompt.timestamp} />
        <button
          className="pending-prompt-dismiss"
          type="button"
          onClick={onCancel}
          aria-label="Cancel queued prompt"
          title="Cancel queued prompt"
        >
          ×
        </button>
      </div>
      {prompt.attachments && prompt.attachments.length > 0 ? (
        <MessageAttachmentList attachments={prompt.attachments} />
      ) : null}
      {prompt.text ? (
        <p className="plain-text-copy">{prompt.text}</p>
      ) : (
        <p className="support-copy">{imageAttachmentSummaryLabel(prompt.attachments?.length ?? 0)}</p>
      )}
    </article>
  );
}, (previous, next) => previous.prompt === next.prompt);

function ConnectionRetryCard({
  message,
  notice,
}: {
  message: TextMessage;
  notice: ConnectionRetryNotice;
}) {
  return (
    <article className="message-card connection-notice-card" role="status" aria-live="polite">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="connection-notice-body">
        <div className="activity-spinner connection-notice-spinner" aria-hidden="true" />
        <div className="connection-notice-copy">
          <div className="card-label">Connection</div>
          <div className="connection-notice-heading">
            <h3>Reconnecting to continue this turn</h3>
            {notice.attemptLabel ? (
              <span className="chip chip-status chip-status-active">{notice.attemptLabel}</span>
            ) : null}
          </div>
          <p className="connection-notice-detail">{notice.detail}</p>
        </div>
      </div>
    </article>
  );
}

function MessageAttachmentList({ attachments }: { attachments: ImageAttachment[] }) {
  return (
    <div className="message-attachment-list">
      {attachments.map((attachment, index) => (
        <div key={`${attachment.fileName}-${attachment.byteSize}-${index}`} className="message-attachment-chip">
          <strong className="message-attachment-name">{attachment.fileName}</strong>
          <span className="message-attachment-meta">
            {formatByteSize(attachment.byteSize)} · {attachment.mediaType}
          </span>
        </div>
      ))}
    </div>
  );
}

function MessageMeta({ author, timestamp }: { author: string; timestamp: string }) {
  return (
    <div className="message-meta">
      <span>{author === "you" ? "You" : "Agent"}</span>
      <span>{timestamp}</span>
    </div>
  );
}

function DeferredHeavyContent({
  children,
  estimatedHeight,
  placeholder,
}: {
  children: ReactNode;
  estimatedHeight: number;
  placeholder: ReactNode;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [isActivated, setIsActivated] = useState(false);

  useLayoutEffect(() => {
    if (isActivated) {
      return;
    }

    const node = containerRef.current;
    if (!node) {
      return;
    }

    const root = resolveDeferredRenderRoot(node);
    if (isElementNearRenderViewport(node, root, DEFERRED_RENDER_ROOT_MARGIN_PX)) {
      setIsActivated(true);
    }
  }, [isActivated]);

  useEffect(() => {
    if (isActivated) {
      return;
    }

    const node = containerRef.current;
    if (!node) {
      return;
    }

    if (typeof window === "undefined" || typeof window.IntersectionObserver === "undefined") {
      setIsActivated(true);
      return;
    }

    const root = resolveDeferredRenderRoot(node);
    const observer = new window.IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting || entry.intersectionRatio > 0)) {
          setIsActivated(true);
        }
      },
      {
        root,
        rootMargin: `${DEFERRED_RENDER_ROOT_MARGIN_PX}px 0px ${DEFERRED_RENDER_ROOT_MARGIN_PX}px 0px`,
        threshold: 0.01,
      },
    );

    observer.observe(node);
    return () => {
      observer.disconnect();
    };
  }, [isActivated]);

  return (
    <div
      ref={containerRef}
      className="deferred-heavy-content"
      style={
        isActivated
          ? undefined
          : ({ "--deferred-min-height": `${estimatedHeight}px` } as CSSProperties)
      }
    >
      {isActivated ? children : placeholder}
    </div>
  );
}

function DeferredHighlightedCodeBlock({
  className,
  code,
  commandHint,
  language,
  pathHint,
}: {
  className: string;
  code: string;
  commandHint?: string | null;
  language?: string | null;
  pathHint?: string | null;
}) {
  const metrics = useMemo(() => measureTextBlock(code), [code]);
  const shouldDefer =
    metrics.lineCount >= HEAVY_CODE_LINE_THRESHOLD || code.length >= HEAVY_CODE_CHARACTER_THRESHOLD;

  if (!shouldDefer) {
    return (
      <HighlightedCodeBlock
        className={className}
        code={code}
        commandHint={commandHint}
        language={language}
        pathHint={pathHint}
      />
    );
  }

  return (
    <DeferredHeavyContent
      estimatedHeight={estimateCodeBlockHeight(metrics.lineCount)}
      placeholder={
        <pre className={`${className} syntax-block deferred-code-placeholder`}>
          <code>{buildDeferredPreviewText(code)}</code>
        </pre>
      }
    >
      <HighlightedCodeBlock
        className={className}
        code={code}
        commandHint={commandHint}
        language={language}
        pathHint={pathHint}
      />
    </DeferredHeavyContent>
  );
}

function DeferredMarkdownContent({ markdown }: { markdown: string }) {
  const metrics = useMemo(() => measureTextBlock(markdown), [markdown]);
  const shouldDefer =
    metrics.lineCount >= HEAVY_MARKDOWN_LINE_THRESHOLD ||
    markdown.length >= HEAVY_MARKDOWN_CHARACTER_THRESHOLD;

  if (!shouldDefer) {
    return <MarkdownContent markdown={markdown} />;
  }

  return (
    <DeferredHeavyContent
      estimatedHeight={estimateMarkdownBlockHeight(metrics.lineCount)}
      placeholder={
        <div className="markdown-copy deferred-markdown-placeholder">
          <p className="plain-text-copy">{buildMarkdownPreviewText(markdown)}</p>
        </div>
      }
    >
      <MarkdownContent markdown={markdown} />
    </DeferredHeavyContent>
  );
}

function HighlightedCodeBlock({
  className,
  code,
  commandHint,
  language,
  pathHint,
}: {
  className: string;
  code: string;
  commandHint?: string | null;
  language?: string | null;
  pathHint?: string | null;
}) {
  const highlighted = useMemo(
    () =>
      highlightCode(code, {
        commandHint,
        language,
        pathHint,
      }),
    [code, commandHint, language, pathHint],
  );

  return (
    <pre className={`${className} syntax-block`}>
      <code
        className={`hljs${highlighted.language ? ` language-${highlighted.language}` : ""}`}
        dangerouslySetInnerHTML={{ __html: highlighted.html }}
      />
    </pre>
  );
}

function ThinkingCard({ message }: { message: ThinkingMessage }) {
  return (
    <article className="message-card reasoning-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Thinking</div>
      <h3>{message.title}</h3>
      <ul className="plain-list">
        {message.lines.map((line) => (
          <li key={line}>{line}</li>
        ))}
      </ul>
    </article>
  );
}

function CommandCard({ message }: { message: CommandMessage }) {
  const [expanded, setExpanded] = useState(false);
  const [copiedSection, setCopiedSection] = useState<"command" | "output" | null>(null);
  const hasOutput = message.output.trim().length > 0;
  const displayOutput = hasOutput
    ? message.output
    : message.status === "running"
      ? "Awaiting output…"
      : "No output";
  const canExpandOutput =
    hasOutput && (message.output.split("\n").length > 10 || message.output.length > 480);
  const statusTone = mapCommandStatus(message.status);

  useEffect(() => {
    if (!copiedSection) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopiedSection(null);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copiedSection]);

  async function handleCopy(section: "command" | "output", text: string) {
    try {
      await copyTextToClipboard(text);
      setCopiedSection(section);
    } catch {
      setCopiedSection(null);
    }
  }

  return (
    <article className="message-card utility-card command-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="utility-header">
        <div>
          <div className="card-label">Command</div>
        </div>
        <div className="utility-actions">
          <span className={`chip chip-status chip-status-${statusTone} command-status-chip`}>
            {message.status}
          </span>
        </div>
      </div>

      <div className="command-panel">
        <div className="command-row">
          <div className="command-row-label">IN</div>
          <div className="command-row-body">
            <DeferredHighlightedCodeBlock
              className="command-text command-text-input"
              code={message.command}
              language={message.commandLanguage ?? "bash"}
            />
          </div>
          <div className="command-row-actions">
            <button
              className={`command-icon-button${copiedSection === "command" ? " copied" : ""}`}
              type="button"
              onClick={() => void handleCopy("command", message.command)}
              aria-label={copiedSection === "command" ? "Command copied" : "Copy command"}
              title={copiedSection === "command" ? "Copied" : "Copy command"}
            >
              {copiedSection === "command" ? <CheckIcon /> : <CopyIcon />}
            </button>
          </div>
        </div>

        <div className="command-row command-row-output">
          <div className="command-row-label">OUT</div>
          <div className="command-row-body">
            <div
              className={`command-output-shell ${expanded ? "expanded" : "collapsed"} ${hasOutput ? "has-output" : "empty"}`}
            >
              {hasOutput ? (
                <DeferredHighlightedCodeBlock
                  className="command-text command-text-output"
                  code={displayOutput}
                  language={message.outputLanguage ?? null}
                  commandHint={message.output ? message.command : null}
                />
              ) : (
                <pre className="command-text command-text-output command-text-placeholder">
                  {displayOutput}
                </pre>
              )}
            </div>
          </div>
          <div className="command-row-actions">
            <button
              className={`command-icon-button${copiedSection === "output" ? " copied" : ""}`}
              type="button"
              onClick={() => void handleCopy("output", message.output)}
              aria-label={copiedSection === "output" ? "Output copied" : "Copy output"}
              title={copiedSection === "output" ? "Copied" : "Copy output"}
              disabled={!message.output}
            >
              {copiedSection === "output" ? <CheckIcon /> : <CopyIcon />}
            </button>
            {canExpandOutput ? (
              <button
                className="command-icon-button"
                type="button"
                onClick={() => setExpanded((open) => !open)}
                aria-label={expanded ? "Collapse output" : "Expand output"}
                aria-pressed={expanded}
                title={expanded ? "Collapse output" : "Expand output"}
              >
                {expanded ? <CollapseIcon /> : <ExpandIcon />}
              </button>
            ) : null}
          </div>
        </div>
      </div>
    </article>
  );
}

function CopyIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M5 2.5h6.5A1.5 1.5 0 0 1 13 4v7.5A1.5 1.5 0 0 1 11.5 13H5A1.5 1.5 0 0 1 3.5 11.5V4A1.5 1.5 0 0 1 5 2.5Zm0 1a.5.5 0 0 0-.5.5v7.5a.5.5 0 0 0 .5.5h6.5a.5.5 0 0 0 .5-.5V4a.5.5 0 0 0-.5-.5H5Z"
        fill="currentColor"
      />
      <path
        d="M2.5 5.5a.5.5 0 0 1 .5.5v6A1.5 1.5 0 0 0 4.5 13.5h5a.5.5 0 0 1 0 1h-5A2.5 2.5 0 0 1 2 12V6a.5.5 0 0 1 .5-.5Z"
        fill="currentColor"
      />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M13.35 4.65a.5.5 0 0 1 0 .7l-6 6a.5.5 0 0 1-.7 0l-3-3a.5.5 0 1 1 .7-.7L7 10.29l5.65-5.64a.5.5 0 0 1 .7 0Z"
        fill="currentColor"
      />
    </svg>
  );
}

function ExpandIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M2.5 6V2.5H6v1H4.2l2.15 2.15-.7.7L3.5 4.2V6h-1Zm11 0V4.2l-2.15 2.15-.7-.7L12.8 3.5H11v-1h3.5V6h-1ZM6.35 10.35l.7.7L4.2 13.9H6v1H2.5v-3.5h1V12.8l2.85-2.45Zm4.6.7.7-.7 2.85 2.45v-1.4h1v3.5H11v-1h1.8l-2.85-2.85Z"
        fill="currentColor"
      />
    </svg>
  );
}

function CollapseIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M6.35 6.35 5.65 7.05 3.5 4.9V6h-1V2.5H6v1H4.9l1.45 1.45Zm3.3 0L11.1 4.9H10v-1h3.5V6h-1V4.9l-2.15 2.15-.7-.7Zm-3.3 3.3.7.7L4.9 12.5H6v1H2.5V10h1v1.1l2.15-2.15Zm3.3.7.7-.7 2.15 2.15V10h1v3.5H10v-1h1.1l-1.45-1.45Z"
        fill="currentColor"
      />
    </svg>
  );
}

function DiffCard({ message }: { message: DiffMessage }) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const canExpandDiff = message.diff.split("\n").length > 14 || message.diff.length > 900;
  const isExpanded = !canExpandDiff || expanded;

  useEffect(() => {
    if (!copied) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopied(false);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copied]);

  async function handleCopy() {
    try {
      await copyTextToClipboard(message.diff);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <article className="message-card utility-card diff-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">{message.changeType === "create" ? "New file" : "File edit"}</div>
      <div className="command-panel diff-panel">
        <div className="command-row diff-file-row">
          <div className="command-row-label">FILE</div>
          <div className="command-row-body">
            <div className="diff-file-path">{message.filePath}</div>
            <p className="diff-file-summary">{message.summary}</p>
          </div>
        </div>
        <div className="command-row diff-row">
          <div className="command-row-label">DIFF</div>
          <div className="command-row-body">
            <div className={`diff-preview-shell ${isExpanded ? "expanded" : "collapsed"}`}>
              <DeferredHighlightedCodeBlock
                className="diff-block diff-preview-text"
                code={message.diff}
                language={message.language ?? "diff"}
                pathHint={message.filePath}
              />
            </div>
          </div>
          <div className="command-row-actions">
            <button
              className={`command-icon-button${copied ? " copied" : ""}`}
              type="button"
              onClick={() => void handleCopy()}
              aria-label={copied ? "Diff copied" : "Copy diff"}
              title={copied ? "Copied" : "Copy diff"}
            >
              {copied ? <CheckIcon /> : <CopyIcon />}
            </button>
            {canExpandDiff ? (
              <button
                className="command-icon-button"
                type="button"
                onClick={() => setExpanded((open) => !open)}
                aria-label={isExpanded ? "Collapse diff" : "Expand diff"}
                aria-pressed={isExpanded}
                title={isExpanded ? "Collapse diff" : "Expand diff"}
              >
                {isExpanded ? <CollapseIcon /> : <ExpandIcon />}
              </button>
            ) : null}
          </div>
        </div>
      </div>
    </article>
  );
}

function MarkdownCard({ message }: { message: MarkdownMessage }) {
  return (
    <article className="message-card markdown-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Markdown</div>
      <h3>{message.title}</h3>
      <DeferredMarkdownContent markdown={message.markdown} />
    </article>
  );
}

function ApprovalCard({
  message,
  onApprovalDecision,
}: {
  message: ApprovalMessage;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
}) {
  const decided = message.decision !== "pending";
  const chosen = (d: ApprovalDecision) => (message.decision === d ? " chosen" : "");
  const resolvedDecision = message.decision === "pending" ? null : message.decision;

  return (
    <article className={`message-card approval-card${decided ? " decided" : ""}`}>
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Approval</div>
      <h3>{message.title}</h3>
      <DeferredHighlightedCodeBlock
        className="approval-command"
        code={message.command}
        language={message.commandLanguage ?? "bash"}
      />
      <p className="support-copy">{message.detail}</p>
      <div className="approval-actions">
        <button
          className={`approval-button${chosen("accepted")}`}
          type="button"
          onClick={() => onApprovalDecision(message.id, "accepted")}
          disabled={decided}
        >
          Approve
        </button>
        <button
          className={`approval-button${chosen("acceptedForSession")}`}
          type="button"
          onClick={() => onApprovalDecision(message.id, "acceptedForSession")}
          disabled={decided}
        >
          Approve for session
        </button>
        <button
          className={`approval-button approval-button-reject${chosen("rejected")}`}
          type="button"
          onClick={() => onApprovalDecision(message.id, "rejected")}
          disabled={decided}
        >
          Reject
        </button>
      </div>
      {resolvedDecision ? (
        <p className="approval-result">Decision: {renderDecision(resolvedDecision)}</p>
      ) : null}
    </article>
  );
}

function MarkdownContent({ markdown }: { markdown: string }) {
  return (
    <div className="markdown-copy">
      <ReactMarkdown
        components={{
          a: ({ href, ...props }) => (
            <a
              {...props}
              href={href}
              target={href?.startsWith("http") ? "_blank" : undefined}
              rel={href?.startsWith("http") ? "noreferrer" : undefined}
            />
          ),
          code: ({ children, className, inline, ...props }) => {
            const language = className?.match(/language-([\w-]+)/)?.[1] ?? null;
            const code = String(children).replace(/\n$/, "");

            if (inline) {
              return (
                <code className={className} {...props}>
                  {children}
                </code>
              );
            }

            return <HighlightedCodeBlock className="code-block" code={code} language={language} />;
          },
        }}
        remarkPlugins={[remarkGfm]}
      >
        {markdown}
      </ReactMarkdown>
    </div>
  );
}

function resolveDeferredRenderRoot(node: Element) {
  const root = node.closest(".message-stack");
  return root instanceof Element ? root : null;
}

function isElementNearRenderViewport(
  node: Element,
  root: Element | null,
  marginPx: number,
) {
  const nodeRect = node.getBoundingClientRect();
  const rootRect = root?.getBoundingClientRect() ?? {
    top: 0,
    bottom: window.innerHeight,
  };

  return nodeRect.bottom >= rootRect.top - marginPx && nodeRect.top <= rootRect.bottom + marginPx;
}

function measureTextBlock(text: string) {
  return {
    lineCount: text.length === 0 ? 1 : text.split("\n").length,
  };
}

function estimateCodeBlockHeight(lineCount: number) {
  return Math.min(MAX_DEFERRED_PLACEHOLDER_HEIGHT, Math.max(120, lineCount * 20 + 48));
}

function estimateMarkdownBlockHeight(lineCount: number) {
  return Math.min(MAX_DEFERRED_PLACEHOLDER_HEIGHT, Math.max(140, lineCount * 28 + 56));
}

function buildDeferredPreviewText(text: string) {
  const preview = text
    .split("\n")
    .slice(0, DEFERRED_PREVIEW_LINE_LIMIT)
    .join("\n")
    .slice(0, DEFERRED_PREVIEW_CHARACTER_LIMIT)
    .trimEnd();

  return preview.length < text.length ? `${preview}\n…` : preview;
}

function buildMarkdownPreviewText(markdown: string) {
  const preview = markdown
    .replace(/```[\s\S]*?```/g, "[code block]")
    .replace(/\[(.*?)\]\((.*?)\)/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/^>\s?/gm, "")
    .replace(/^[*-]\s+/gm, "")
    .replace(/`/g, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();

  return buildDeferredPreviewText(preview);
}

function renderDecision(decision: Exclude<ApprovalDecision, "pending">) {
  switch (decision) {
    case "accepted":
      return "approved";
    case "acceptedForSession":
      return "approved for this session";
    case "rejected":
      return "rejected";
  }
}

function labelForStatus(status: Session["status"]) {
  switch (status) {
    case "active":
      return "Active";
    case "idle":
      return "Idle";
    case "approval":
      return "Awaiting approval";
    case "error":
      return "Error";
  }
}

function labelForPaneViewMode(viewMode: PaneViewMode) {
  switch (viewMode) {
    case "session":
      return "Session";
    case "prompt":
      return "Prompt";
    case "commands":
      return "Commands";
    case "diffs":
      return "Diffs";
    case "source":
      return "Source";
  }
}

function mapCommandStatus(status: CommandMessage["status"]): Session["status"] {
  switch (status) {
    case "success":
      return "idle";
    case "running":
      return "active";
    case "error":
      return "error";
  }
}

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The request failed.";
}

type ConnectionRetryNotice = {
  attemptLabel: string | null;
  detail: string;
};

function parseConnectionRetryNotice(text: string): ConnectionRetryNotice | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("Connection dropped before the response finished.")) {
    return null;
  }

  const attemptMatch = trimmed.match(/Retrying automatically \(attempt (\d+) of (\d+)\)\.?$/);
  const attemptLabel = attemptMatch ? `Attempt ${attemptMatch[1]} of ${attemptMatch[2]}` : null;

  return {
    attemptLabel,
    detail: trimmed,
  };
}

function buildMessageListSignature(messages: Message[]) {
  const lastMessage = messages[messages.length - 1];

  return [
    messages.length.toString(),
    lastMessage?.id ?? "no-message",
    lastMessage ? messageChangeMarker(lastMessage) : "empty",
  ].join("|");
}

function buildSessionConversationItems(session: Session): SessionConversationItem[] {
  return [
    ...session.messages.map((message) => ({
      author: message.author,
      id: message.id,
      kind: "message" as const,
      message,
    })),
    ...(session.pendingPrompts ?? []).map((prompt) => ({
      author: "you" as const,
      id: prompt.id,
      kind: "pendingPrompt" as const,
      prompt,
    })),
  ];
}

function buildConversationListSignature(items: SessionConversationItem[]) {
  const lastMessageItem = [...items].reverse().find((item) => item.kind === "message");
  const lastPendingPromptItem = [...items].reverse().find((item) => item.kind === "pendingPrompt");

  return [
    items.length.toString(),
    lastMessageItem?.id ?? "no-message",
    lastMessageItem?.kind === "message" ? messageChangeMarker(lastMessageItem.message) : "empty",
    lastPendingPromptItem?.id ?? "no-pending-prompt",
    lastPendingPromptItem?.kind === "pendingPrompt"
      ? pendingPromptChangeMarker(lastPendingPromptItem.prompt)
      : "empty",
  ].join("|");
}

function buildVirtualizedMessageLayout(itemHeights: number[]) {
  const tops = new Array<number>(itemHeights.length);
  let offset = 0;

  for (let index = 0; index < itemHeights.length; index += 1) {
    tops[index] = offset;
    offset += itemHeights[index] + VIRTUALIZED_MESSAGE_GAP_PX;
  }

  return {
    tops,
    totalHeight: Math.max(offset - VIRTUALIZED_MESSAGE_GAP_PX, 0),
  };
}

function findVirtualizedMessageRange(
  tops: number[],
  itemHeights: number[],
  scrollTop: number,
  viewportHeight: number,
  overscan: number,
) {
  if (itemHeights.length === 0) {
    return {
      startIndex: 0,
      endIndex: 0,
    };
  }

  const startBoundary = Math.max(scrollTop - overscan, 0);
  const endBoundary = scrollTop + Math.max(viewportHeight, DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT) + overscan;

  let startIndex = 0;
  while (
    startIndex < itemHeights.length - 1 &&
    tops[startIndex] + itemHeights[startIndex] < startBoundary
  ) {
    startIndex += 1;
  }

  let endIndex = startIndex;
  while (endIndex < itemHeights.length && tops[endIndex] < endBoundary) {
    endIndex += 1;
  }

  return {
    startIndex,
    endIndex: Math.max(startIndex + 1, endIndex),
  };
}

function estimateConversationMessageHeight(message: Message) {
  switch (message.type) {
    case "text": {
      const lineCount = measureTextBlock(message.text).lineCount;
      const attachmentHeight = (message.attachments?.length ?? 0) * 54;
      return Math.min(1800, Math.max(92, 78 + lineCount * 24 + attachmentHeight));
    }
    case "thinking":
      return Math.min(900, Math.max(140, 112 + message.lines.length * 28));
    case "command": {
      const commandLineCount = measureTextBlock(message.command).lineCount;
      const outputLineCount = message.output ? measureTextBlock(message.output).lineCount : 3;
      return Math.min(1400, Math.max(180, 152 + commandLineCount * 22 + Math.min(outputLineCount, 14) * 20));
    }
    case "diff": {
      const diffLineCount = measureTextBlock(message.diff).lineCount;
      return Math.min(1500, Math.max(180, 156 + Math.min(diffLineCount, 20) * 20));
    }
    case "markdown": {
      const markdownLineCount = measureTextBlock(message.markdown).lineCount;
      return Math.min(1600, Math.max(140, 124 + markdownLineCount * 24));
    }
    case "approval":
      return Math.max(220, 188 + measureTextBlock(message.detail).lineCount * 22);
  }
}

function messageChangeMarker(message: Message) {
  switch (message.type) {
    case "text":
      return `${message.type}:${message.text.length}:${message.attachments?.length ?? 0}`;
    case "thinking":
      return `${message.type}:${message.lines.length}:${message.title.length}`;
    case "command":
      return `${message.type}:${message.status}:${message.output.length}`;
    case "diff":
      return `${message.type}:${message.filePath}:${message.diff.length}`;
    case "markdown":
      return `${message.type}:${message.title.length}:${message.markdown.length}`;
    case "approval":
      return `${message.type}:${message.decision}:${message.command.length}`;
  }
}

function pendingPromptChangeMarker(prompt: PendingPrompt) {
  return `${prompt.text.length}:${prompt.attachments?.length ?? 0}`;
}

function formatRateLimitResetLabel(resetsAt: number | null, label: string) {
  if (!resetsAt) {
    return null;
  }

  const date = new Date(resetsAt * 1000);
  if (Number.isNaN(date.getTime())) {
    return null;
  }

  const formatter =
    label === "5h limit"
      ? new Intl.DateTimeFormat(undefined, {
          hour: "numeric",
          minute: "2-digit",
        })
      : new Intl.DateTimeFormat(undefined, {
          month: "short",
          day: "numeric",
        });

  return `resets ${formatter.format(date)}`;
}

function collectCandidateSourcePaths(session: Session) {
  const paths = session.messages
    .filter((message): message is DiffMessage => message.type === "diff")
    .map((message) => message.filePath);

  return Array.from(new Set(paths));
}

function collectUserPromptHistory(session: Session) {
  return session.messages.flatMap((message) => {
    if (message.type !== "text" || message.author !== "you") {
      return [];
    }

    const prompt = message.text.trim();
    return prompt ? [prompt] : [];
  });
}

function findLastUserPrompt(session: Session) {
  for (let index = session.messages.length - 1; index >= 0; index -= 1) {
    const message = session.messages[index];
    if (message.type === "text" && message.author === "you") {
      const prompt = message.text.trim();
      if (prompt) {
        return prompt;
      }
    }
  }

  return null;
}

function setSessionFlag(current: SessionFlagMap, sessionId: string, value: boolean): SessionFlagMap {
  if (value) {
    return {
      ...current,
      [sessionId]: true,
    };
  }

  if (!current[sessionId]) {
    return current;
  }

  const next = { ...current };
  delete next[sessionId];
  return next;
}

function pruneSessionValues(
  current: Record<string, string>,
  availableSessionIds: Set<string>,
): Record<string, string> {
  const nextEntries = Object.entries(current).filter(([sessionId]) => availableSessionIds.has(sessionId));
  return Object.fromEntries(nextEntries);
}

function pruneSessionAttachmentValues(
  current: Record<string, DraftImageAttachment[]>,
  availableSessionIds: Set<string>,
): Record<string, DraftImageAttachment[]> {
  const nextEntries = Object.entries(current).filter(([sessionId]) => {
    const keep = availableSessionIds.has(sessionId);
    if (!keep) {
      releaseDraftAttachments(current[sessionId] ?? []);
    }
    return keep;
  });
  return Object.fromEntries(nextEntries);
}

function pruneSessionFlags(current: SessionFlagMap, availableSessionIds: Set<string>): SessionFlagMap {
  const nextEntries = Object.entries(current).filter(([sessionId]) => availableSessionIds.has(sessionId));
  return Object.fromEntries(nextEntries);
}

function removeQueuedPromptFromSessions(
  sessions: Session[],
  sessionId: string,
  promptId: string,
): Session[] {
  return sessions.map((session) => {
    if (session.id !== sessionId || !session.pendingPrompts?.length) {
      return session;
    }

    const pendingPrompts = session.pendingPrompts.filter((prompt) => prompt.id !== promptId);
    if (pendingPrompts.length === session.pendingPrompts.length) {
      return session;
    }

    if (pendingPrompts.length === 0) {
      const { pendingPrompts: _discard, ...rest } = session;
      return rest;
    }

    return {
      ...session,
      pendingPrompts,
    };
  });
}

function releaseDraftAttachments(attachments: DraftImageAttachment[]) {
  for (const attachment of attachments) {
    URL.revokeObjectURL(attachment.previewUrl);
  }
}

function collectClipboardImageFiles(clipboardData: DataTransfer | null): File[] {
  if (!clipboardData) {
    return [];
  }

  return Array.from(clipboardData.items)
    .filter((item) => item.kind === "file")
    .map((item) => item.getAsFile())
    .filter((file): file is File => file !== null && file.type.startsWith("image/"));
}

async function createDraftAttachmentsFromFiles(files: File[]) {
  const attachments: DraftImageAttachment[] = [];
  const errors: string[] = [];

  for (const [index, file] of files.entries()) {
    try {
      attachments.push(await createDraftAttachment(file, index));
    } catch (error) {
      errors.push(getErrorMessage(error));
    }
  }

  return { attachments, errors };
}

async function createDraftAttachment(file: File, index: number): Promise<DraftImageAttachment> {
  const mediaType = file.type.trim().toLowerCase();
  if (!SUPPORTED_PASTED_IMAGE_TYPES.has(mediaType)) {
    throw new Error(`Unsupported pasted image type \`${mediaType || "unknown"}\`.`);
  }

  if (file.size > MAX_PASTED_IMAGE_BYTES) {
    throw new Error(
      `Pasted image exceeds the ${Math.round(MAX_PASTED_IMAGE_BYTES / (1024 * 1024))} MB limit.`,
    );
  }

  const dataUrl = await readFileAsDataUrl(file);
  const [, base64Data = ""] = dataUrl.split(",", 2);
  if (!base64Data) {
    throw new Error("Failed to read pasted image data.");
  }

  const fileName = file.name.trim() || defaultDraftAttachmentFileName(index, mediaType);

  return {
    id: crypto.randomUUID(),
    previewUrl: URL.createObjectURL(file),
    base64Data,
    byteSize: file.size,
    fileName,
    mediaType,
  };
}

function readFileAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => {
      reject(new Error(`Failed to read pasted image \`${file.name || "image"}\`.`));
    };
    reader.onload = () => {
      if (typeof reader.result === "string") {
        resolve(reader.result);
      } else {
        reject(new Error(`Failed to decode pasted image \`${file.name || "image"}\`.`));
      }
    };
    reader.readAsDataURL(file);
  });
}

function defaultDraftAttachmentFileName(index: number, mediaType: string) {
  const extension = mediaType === "image/png"
    ? "png"
    : mediaType === "image/jpeg"
      ? "jpg"
      : mediaType === "image/gif"
        ? "gif"
        : mediaType === "image/webp"
          ? "webp"
          : "img";

  return `pasted-image-${index + 1}.${extension}`;
}

function imageAttachmentSummaryLabel(count: number) {
  return count === 1 ? "1 image attached" : `${count} images attached`;
}

function formatByteSize(byteSize: number) {
  if (byteSize < 1024) {
    return `${byteSize} B`;
  }

  if (byteSize < 1024 * 1024) {
    return `${(byteSize / 1024).toFixed(1)} KB`;
  }

  return `${(byteSize / (1024 * 1024)).toFixed(1)} MB`;
}

async function copyTextToClipboard(text: string) {
  if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(text);
    return;
  }

  if (typeof document === "undefined") {
    throw new Error("Clipboard is unavailable.");
  }

  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.setAttribute("readonly", "");
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  textarea.style.pointerEvents = "none";
  document.body.appendChild(textarea);
  textarea.select();

  const didCopy = document.execCommand("copy");
  document.body.removeChild(textarea);

  if (!didCopy) {
    throw new Error("Clipboard copy failed.");
  }
}

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}

function dropLabelForPlacement(placement: TabDropPlacement) {
  switch (placement) {
    case "tabs":
      return "Tabs";
    case "left":
      return "Left";
    case "right":
      return "Right";
    case "top":
      return "Top";
    case "bottom":
      return "Bottom";
  }
}

