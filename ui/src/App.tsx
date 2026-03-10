import {
  memo,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ClipboardEvent as ReactClipboardEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
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
  updateSessionSettings,
} from "./api";
import { highlightCode } from "./highlight";
import type {
  ApprovalDecision,
  ApprovalMessage,
  ApprovalPolicy,
  AgentType,
  ClaudeApprovalMode,
  CommandMessage,
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

type SessionFlagMap = Record<string, true | undefined>;
type SessionSettingsField = "sandboxMode" | "approvalPolicy" | "claudeApprovalMode";
type SessionSettingsValue = SandboxMode | ApprovalPolicy | ClaudeApprovalMode;
type PreferencesTabId = "themes" | "claude-approvals";
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
  { id: "claude-approvals", label: "Claude approvals" },
];

type ComboboxOption = {
  label: string;
  value: string;
};

export default function App() {
  const [sessions, setSessions] = useState<Session[]>([]);
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
  const [themeId, setThemeId] = useState<ThemeId>(() => getStoredThemePreference());
  const [defaultClaudeApprovalMode, setDefaultClaudeApprovalMode] =
    useState<ClaudeApprovalMode>("ask");
  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState<PreferencesTabId>("themes");
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

  const sessionLookup = new Map(sessions.map((session) => [session.id, session]));
  const paneLookup = new Map(workspace.panes.map((pane) => [pane.id, pane]));
  const activePane =
    workspace.panes.find((pane) => pane.id === workspace.activePaneId) ?? workspace.panes[0] ?? null;
  const activeSession = activePane?.activeSessionId
    ? (sessionLookup.get(activePane.activeSessionId) ?? null)
    : null;
  const openSessionIds = new Set(workspace.panes.flatMap((pane) => pane.sessionIds));
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

  useEffect(() => {
    let cancelled = false;
    const eventSource = new EventSource("/api/events");

    async function loadInitialState() {
      try {
        const state = await fetchState();
        if (cancelled) {
          return;
        }

        adoptSessions(state.sessions);
        setRequestError(null);
      } catch (error) {
        if (cancelled) {
          return;
        }

        setRequestError(getErrorMessage(error));
      } finally {
        if (!cancelled) {
          setIsLoading(false);
        }
      }
    }

    function handleStateEvent(event: MessageEvent<string>) {
      if (cancelled) {
        return;
      }

      try {
        const state = JSON.parse(event.data) as { sessions: Session[] };
        adoptSessions(state.sessions);
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

    eventSource.addEventListener("state", handleStateEvent as EventListener);
    eventSource.onopen = () => {
      if (!cancelled) {
        setRequestError(null);
      }
    };

    void loadInitialState();

    return () => {
      cancelled = true;
      eventSource.removeEventListener("state", handleStateEvent as EventListener);
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

  async function handleSend(sessionId: string) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return;
    }

    const draftText = draftsBySessionId[sessionId] ?? "";
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
      adoptSessions(state.sessions);
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
        claudeApprovalMode:
          newSessionAgent === "Claude" ? defaultClaudeApprovalMode : undefined,
        workdir: activeSession?.workdir,
      });
      const state = await fetchState();
      adoptSessions(state.sessions, {
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
      adoptSessions(state.sessions);
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
      adoptSessions(state.sessions);
      setRequestError(null);
    } catch (error) {
      try {
        const state = await fetchState();
        adoptSessions(state.sessions);
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
      adoptSessions(state.sessions);
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
      adoptSessions(state.sessions);
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
      adoptSessions(state.sessions);
      setRequestError(null);
    } catch (error) {
      setRequestError(getErrorMessage(error));
    } finally {
      setUpdatingSessionIds((current) => setSessionFlag(current, sessionId, false));
    }
  }

  function handleSidebarSessionClick(sessionId: string) {
    setKillRevealSessionId(null);
    setWorkspace((current) => openSessionInWorkspaceState(current, sessionId, current.activePaneId));
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
    setDraftsBySessionId((current) => ({
      ...current,
      [sessionId]: nextValue,
    }));
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

  function handleTabDrop(targetPaneId: string, placement: TabDropPlacement) {
    if (!draggedTab) {
      return;
    }

    setWorkspace((current) =>
      placeDraggedSession(current, draggedTab.sourcePaneId, draggedTab.sessionId, targetPaneId, placement),
    );
    setDraggedTab(null);
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
          {sessions.map((session) => {
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
          })}
        </div>
      </aside>

      <main className="workspace-shell">
        <section className="workspace-overview panel">
          <p className="eyebrow workspace-label">Workspace</p>
          <div className="thread-chips">
            <span className="chip">{workspace.panes.length} tiles</span>
            <span className="chip">{openSessionIds.size} open sessions</span>
            <span className="chip">{sessions.length} total sessions</span>
          </div>
        </section>

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
              onDraftChange={handleDraftChange}
              onDraftAttachmentsAdd={handleDraftAttachmentsAdd}
              onDraftAttachmentRemove={handleDraftAttachmentRemove}
              onComposerError={setRequestError}
              onSend={handleSend}
              onCancelQueuedPrompt={handleCancelQueuedPrompt}
              onApprovalDecision={handleApprovalDecision}
              onStopSession={handleStopSession}
              onKillSession={handleKillSession}
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
  onDraftChange,
  onDraftAttachmentsAdd,
  onDraftAttachmentRemove,
  onComposerError,
  onSend,
  onCancelQueuedPrompt,
  onApprovalDecision,
  onStopSession,
  onKillSession,
  onSessionSettingsChange,
}: {
  node: WorkspaceNode;
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
  onTabDrop: (targetPaneId: string, placement: TabDropPlacement) => void;
  onPaneViewModeChange: (paneId: string, viewMode: PaneViewMode) => void;
  onPaneSourcePathChange: (paneId: string, path: string) => void;
  onDraftChange: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentsAdd: (sessionId: string, attachments: DraftImageAttachment[]) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onComposerError: (message: string | null) => void;
  onSend: (sessionId: string) => void;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onKillSession: (sessionId: string) => void;
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
        onDraftChange={onDraftChange}
        onDraftAttachmentsAdd={onDraftAttachmentsAdd}
        onDraftAttachmentRemove={onDraftAttachmentRemove}
        onComposerError={onComposerError}
        onSend={onSend}
        onCancelQueuedPrompt={onCancelQueuedPrompt}
        onApprovalDecision={onApprovalDecision}
        onStopSession={onStopSession}
        onKillSession={onKillSession}
        onSessionSettingsChange={onSessionSettingsChange}
      />
    );
  }

  return (
    <div className={`tile-split tile-split-${node.direction}`}>
      <div className="tile-branch" style={{ flexGrow: node.ratio, flexBasis: 0 }}>
        <WorkspaceNodeView
          node={node.first}
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
          onDraftChange={onDraftChange}
          onDraftAttachmentsAdd={onDraftAttachmentsAdd}
          onDraftAttachmentRemove={onDraftAttachmentRemove}
          onComposerError={onComposerError}
          onSend={onSend}
          onCancelQueuedPrompt={onCancelQueuedPrompt}
          onApprovalDecision={onApprovalDecision}
          onStopSession={onStopSession}
          onKillSession={onKillSession}
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
          onDraftChange={onDraftChange}
          onDraftAttachmentsAdd={onDraftAttachmentsAdd}
          onDraftAttachmentRemove={onDraftAttachmentRemove}
          onComposerError={onComposerError}
          onSend={onSend}
          onCancelQueuedPrompt={onCancelQueuedPrompt}
          onApprovalDecision={onApprovalDecision}
          onStopSession={onStopSession}
          onKillSession={onKillSession}
          onSessionSettingsChange={onSessionSettingsChange}
        />
      </div>
    </div>
  );
}

function SessionPaneView({
  pane,
  sessions,
  isActive,
  isLoading,
  draft,
  draftAttachments,
  isSending,
  isStopping,
  isKilling,
  isUpdating,
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
  onDraftChange,
  onDraftAttachmentsAdd,
  onDraftAttachmentRemove,
  onComposerError,
  onSend,
  onCancelQueuedPrompt,
  onApprovalDecision,
  onStopSession,
  onKillSession,
  onSessionSettingsChange,
}: {
  pane: WorkspacePane;
  sessions: Session[];
  isActive: boolean;
  isLoading: boolean;
  draft: string;
  draftAttachments: DraftImageAttachment[];
  isSending: boolean;
  isStopping: boolean;
  isKilling: boolean;
  isUpdating: boolean;
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
  onTabDrop: (targetPaneId: string, placement: TabDropPlacement) => void;
  onPaneViewModeChange: (paneId: string, viewMode: PaneViewMode) => void;
  onPaneSourcePathChange: (paneId: string, path: string) => void;
  onDraftChange: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentsAdd: (sessionId: string, attachments: DraftImageAttachment[]) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onComposerError: (message: string | null) => void;
  onSend: (sessionId: string) => void;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onKillSession: (sessionId: string) => void;
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
  }>({
    status: "idle",
    path: "",
    content: "",
    error: null,
  });
  const messageStackRef = useRef<HTMLElement | null>(null);
  const paneTabsRef = useRef<HTMLDivElement | null>(null);
  const composerInputRef = useRef<HTMLTextAreaElement | null>(null);
  const shouldStickToBottomRef = useRef(true);
  const scrollPositionsRef = useRef<Record<string, { top: number; shouldStick: boolean }>>({});
  const contentSignaturesRef = useRef<Record<string, string>>({});
  const [tabRailState, setTabRailState] = useState({
    hasOverflow: false,
    canScrollPrev: false,
    canScrollNext: false,
  });
  const [activeDropPlacement, setActiveDropPlacement] = useState<TabDropPlacement | null>(null);
  const [promptHistoryStateBySessionId, setPromptHistoryStateBySessionId] = useState<
    Record<string, PromptHistoryState | undefined>
  >({});
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

  function handleComposerChange(nextValue: string) {
    if (!activeSession) {
      return;
    }

    resetPromptHistory(activeSession.id);
    onDraftChange(activeSession.id, nextValue);
  }

  function handleComposerSend() {
    if (!activeSession) {
      return;
    }

    resetPromptHistory(activeSession.id);
    onSend(activeSession.id);
    window.requestAnimationFrame(() => {
      composerInputRef.current?.focus();
    });
  }

  function handleComposerKeyDown(event: ReactKeyboardEvent<HTMLTextAreaElement>) {
    if (!activeSession) {
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

    const promptHistory = collectUserPromptHistory(activeSession);
    if (promptHistory.length === 0) {
      return;
    }

    const historyState = promptHistoryStateBySessionId[activeSession.id];
    if (event.key === "ArrowDown" && !historyState) {
      return;
    }

    event.preventDefault();

    if (event.key === "ArrowUp") {
      const nextIndex = historyState ? Math.max(historyState.index - 1, 0) : promptHistory.length - 1;
      const draftSnapshot = historyState?.draft ?? draft;

      setPromptHistoryStateBySessionId((current) => ({
        ...current,
        [activeSession.id]: {
          index: nextIndex,
          draft: draftSnapshot,
        },
      }));
      onDraftChange(activeSession.id, promptHistory[nextIndex]);
    } else {
      const currentHistoryState = historyState;
      if (!currentHistoryState) {
        return;
      }

      if (currentHistoryState.index >= promptHistory.length - 1) {
        resetPromptHistory(activeSession.id);
        onDraftChange(activeSession.id, currentHistoryState.draft);
      } else {
        const nextIndex = currentHistoryState.index + 1;
        setPromptHistoryStateBySessionId((current) => ({
          ...current,
          [activeSession.id]: {
            index: nextIndex,
            draft: currentHistoryState.draft,
          },
        }));
        onDraftChange(activeSession.id, promptHistory[nextIndex]);
      }
    }

    window.requestAnimationFrame(() => {
      textarea.setSelectionRange(0, 0);
    });
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
    shouldStickToBottomRef.current = true;
    scrollPositionsRef.current[scrollStateKey] = {
      top: node.scrollHeight,
      shouldStick: true,
    };
    setNewResponseIndicator(scrollStateKey, false);
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

  useLayoutEffect(() => {
    const frameId = window.requestAnimationFrame(() => {
      const node = messageStackRef.current;
      if (!node) {
        return;
      }

      const saved = scrollPositionsRef.current[scrollStateKey];
      if (saved) {
        const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
        node.scrollTop = Math.min(saved.top, maxScrollTop);
        shouldStickToBottomRef.current = saved.shouldStick;
        return;
      }

      if (defaultScrollToBottom) {
        node.scrollTop = node.scrollHeight;
        shouldStickToBottomRef.current = true;
        scrollPositionsRef.current[scrollStateKey] = {
          top: node.scrollTop,
          shouldStick: true,
        };
        return;
      }

      node.scrollTop = 0;
      shouldStickToBottomRef.current = false;
      scrollPositionsRef.current[scrollStateKey] = {
        top: 0,
        shouldStick: false,
      };
    });

    return () => {
      window.cancelAnimationFrame(frameId);
    };
  }, [defaultScrollToBottom, scrollStateKey]);

  useEffect(() => {
    if (!activeSession || pane.viewMode === "source") {
      return;
    }

    const previousSignature = contentSignaturesRef.current[scrollStateKey];
    contentSignaturesRef.current[scrollStateKey] = visibleContentSignature;
    if (previousSignature === undefined || previousSignature === visibleContentSignature) {
      return;
    }

    const shouldScroll =
      shouldStickToBottomRef.current ||
      scrollPositionsRef.current[scrollStateKey]?.shouldStick === true ||
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

  return (
    <section
      className={`workspace-pane thread panel ${isActive ? "active" : ""}`}
      onMouseDown={() => onActivatePane(pane.id)}
    >
      {showDropOverlay ? (
        <div className="pane-drop-overlay">
          {(["tabs", "left", "top", "right", "bottom"] as TabDropPlacement[]).map((placement) => (
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
                setActiveDropPlacement((current) => (current === placement ? null : current));
              }}
              onDrop={(event) => {
                event.preventDefault();
                setActiveDropPlacement(null);
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
              <div ref={paneTabsRef} className="pane-tabs" role="tablist" aria-label="Tile sessions">
                {sessions.length > 0 ? (
                  sessions.map((session) => {
                    const tabActive = session.id === activeSession?.id;

                    return (
                      <div
                        key={session.id}
                        className={`pane-tab-shell ${tabActive ? "active" : ""}`}
                        role="tab"
                        aria-selected={tabActive}
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
          shouldStickToBottomRef.current = shouldStick;
          scrollPositionsRef.current[scrollStateKey] = {
            top: node.scrollTop,
            shouldStick,
          };
          if (shouldStick) {
            setNewResponseIndicator(scrollStateKey, false);
          }
        }}
      >
        {activeSession ? (
          pane.viewMode === "session" ? (
            activeSession.messages.length > 0 || pendingPrompts.length > 0 || showWaitingIndicator ? (
              <>
                {activeSession.messages.map((message) => (
                  <MessageCard
                    key={message.id}
                    message={message}
                    onApprovalDecision={(messageId, decision) =>
                      onApprovalDecision(activeSession.id, messageId, decision)
                    }
                  />
                ))}

                {showWaitingIndicator ? (
                  <RunningIndicator agent={activeSession.agent} lastPrompt={waitingIndicatorPrompt} />
                ) : null}

                {pendingPrompts.map((prompt) => (
                  <PendingPromptCard
                    key={prompt.id}
                    prompt={prompt}
                    onCancel={() => onCancelQueuedPrompt(activeSession.id, prompt.id)}
                  />
                ))}
              </>
            ) : (
              <EmptyState
                title={isLoading ? "Connecting to backend" : "Live session is ready"}
                body={
                  isLoading
                    ? "Fetching session state from the Rust backend."
                    : `Send a prompt to ${activeSession.agent} and this tile will fill with live cards.`
                }
              />
            )
          ) : pane.viewMode === "prompt" ? (
            activeSession.agent === "Codex" ? (
              <CodexPromptSettingsCard
                paneId={pane.id}
                session={activeSession}
                isUpdating={isUpdating}
                onSessionSettingsChange={onSessionSettingsChange}
              />
            ) : activeSession.agent === "Claude" ? (
              <ClaudePromptSettingsCard
                paneId={pane.id}
                session={activeSession}
                isUpdating={isUpdating}
                onSessionSettingsChange={onSessionSettingsChange}
              />
            ) : (
              <EmptyState
                title="No prompt settings"
                body="Prompt controls are only available for supported agent sessions."
              />
            )
          ) : pane.viewMode === "commands" ? (
            commandMessages.length > 0 ? (
              commandMessages.map((message) => <CommandCard key={message.id} message={message} />)
            ) : (
              <EmptyState
                title="No commands yet"
                body="This tile is filtered to command executions. Send a prompt that runs tools and they will show up here."
              />
            )
          ) : pane.viewMode === "diffs" ? (
            diffMessages.length > 0 ? (
              diffMessages.map((message) => <DiffCard key={message.id} message={message} />)
            ) : (
              <EmptyState
                title="No diffs yet"
                body="This tile is filtered to file changes. When the agent edits or creates files, the diffs will appear here."
              />
            )
          ) : (
            <SourcePane
              candidatePaths={candidateSourcePaths}
              fileState={fileState}
              sourceDraft={sourceDraft}
              sourcePath={pane.sourcePath}
              onDraftChange={setSourceDraft}
              onOpenPath={(path) => onPaneSourcePathChange(pane.id, path)}
            />
          )
        ) : (
          <EmptyState
            title="Ready for a session"
            body="Click a session on the left to open it in the active tile."
          />
        )}
      </section>

      {pane.viewMode === "session" ? (
        <footer className="composer">
          {showNewResponseIndicator ? (
            <button
              className="new-response-indicator"
              type="button"
              onClick={() => scrollToLatestMessage("smooth")}
            >
              New response
            </button>
          ) : null}
          <label className="composer-label" htmlFor={`prompt-${pane.id}`}>
            Message {activeSession?.name ?? "session"}
          </label>
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
                    onClick={() => activeSession && onDraftAttachmentRemove(activeSession.id, attachment.id)}
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
              id={`prompt-${pane.id}`}
              ref={composerInputRef}
              className="composer-input"
              value={draft}
              onChange={(event) => handleComposerChange(event.target.value)}
              disabled={composerInputDisabled}
              onKeyDown={handleComposerKeyDown}
              onPaste={handleComposerPaste}
              placeholder={activeSession ? `Send a prompt to ${activeSession.agent}...` : "Open a session..."}
              rows={3}
            />
            <div className="composer-actions">
              {activeSession && (isSessionBusy || isStopping) ? (
                <button
                  className="ghost-button composer-stop-button"
                  type="button"
                  onClick={() => onStopSession(activeSession.id)}
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
          {activeSession ? (
            <p className="composer-hint">
              Paste images into the composer to attach them to the next prompt.
              {isSessionBusy ? " Send will queue a follow-up while this turn is running." : ""}
            </p>
          ) : null}
        </footer>
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
          <HighlightedCodeBlock
            className="code-block source-code-block"
            code={fileState.content}
            pathHint={fileState.path}
          />
        </article>
      ) : null}
    </div>
  );
}

const MessageCard = memo(function MessageCard({
  message,
  onApprovalDecision,
}: {
  message: Message;
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
            <MarkdownContent markdown={message.text} />
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
}, (previous, next) => previous.message === next.message);

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
            <HighlightedCodeBlock
              className="command-text command-text-input"
              code={message.command}
              language="bash"
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
              <HighlightedCodeBlock
                className="command-text command-text-output"
                code={displayOutput}
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
              <HighlightedCodeBlock
                className="diff-block diff-preview-text"
                code={message.diff}
                language="diff"
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
      <MarkdownContent markdown={message.markdown} />
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
      <HighlightedCodeBlock className="approval-command" code={message.command} language="bash" />
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
