import {
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ClipboardEvent as ReactClipboardEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
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
  ThinkingMessage,
} from "./types";

type WorkspacePane = {
  id: string;
  sessionIds: string[];
  activeSessionId: string | null;
  viewMode: PaneViewMode;
  sourcePath: string | null;
};

type WorkspaceNode =
  | {
      type: "pane";
      paneId: string;
    }
  | {
      id: string;
      type: "split";
      direction: "row" | "column";
      ratio: number;
      first: WorkspaceNode;
      second: WorkspaceNode;
    };

type WorkspaceState = {
  root: WorkspaceNode | null;
  panes: WorkspacePane[];
  activePaneId: string | null;
};

type SessionFlagMap = Record<string, true | undefined>;
type TabDropPlacement = "left" | "right" | "top" | "bottom" | "tabs";
type PaneViewMode = "session" | "prompt" | "commands" | "diffs" | "source";
type SessionSettingsField = "sandboxMode" | "approvalPolicy" | "claudeApprovalMode";
type SessionSettingsValue = SandboxMode | ApprovalPolicy | ClaudeApprovalMode;
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

  const sessionLookup = new Map(sessions.map((session) => [session.id, session]));
  const paneLookup = new Map(workspace.panes.map((pane) => [pane.id, pane]));
  const activePane =
    workspace.panes.find((pane) => pane.id === workspace.activePaneId) ?? workspace.panes[0] ?? null;
  const activeSession = activePane?.activeSessionId
    ? (sessionLookup.get(activePane.activeSessionId) ?? null)
    : null;
  const openSessionIds = new Set(workspace.panes.flatMap((pane) => pane.sessionIds));

  function adoptSessions(
    nextSessions: Session[],
    options?: { openSessionId?: string; paneId?: string | null },
  ) {
    const availableSessionIds = new Set(nextSessions.map((session) => session.id));

    setSessions(nextSessions);
    setWorkspace((current) => {
      const reconciled = reconcileWorkspaceState(current, nextSessions);
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
    setSessions((current) => removeQueuedPromptFromSessions(current, sessionId, promptId));
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
            <select
              id="new-session-agent"
              className="session-select new-session-agent-select"
              value={newSessionAgent}
              onChange={(event) => setNewSessionAgent(event.target.value as AgentType)}
              disabled={isCreating}
            >
              <option value="Claude">Claude</option>
              <option value="Codex">Codex</option>
            </select>
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
    </div>
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
  const candidateSourcePaths = activeSession ? collectCandidateSourcePaths(activeSession) : [];
  const commandMessages = activeSession
    ? activeSession.messages.filter((message): message is CommandMessage => message.type === "command")
    : [];
  const diffMessages = activeSession
    ? activeSession.messages.filter((message): message is DiffMessage => message.type === "diff")
    : [];
  const pendingPrompts = activeSession?.pendingPrompts ?? [];
  const sessionConversationItems = activeSession ? buildSessionConversationItems(activeSession) : [];
  const lastUserPrompt = activeSession ? findLastUserPrompt(activeSession) : null;
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
  const visibleMessages =
    pane.viewMode === "commands"
      ? commandMessages
      : pane.viewMode === "diffs"
        ? diffMessages
        : [];
  const visibleContentSignature =
    pane.viewMode === "session"
      ? buildConversationListSignature(sessionConversationItems)
      : buildMessageListSignature(visibleMessages);
  const visibleLastMessageAuthor =
    pane.viewMode === "session"
      ? activeSession?.messages[activeSession.messages.length - 1]?.author
      : visibleMessages[visibleMessages.length - 1]?.author;
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
          <select
            id={`sandbox-mode-${paneId}`}
            className="session-select"
            value={session.sandboxMode ?? "workspace-write"}
            disabled={isUpdating}
            onChange={(event) =>
              void onSessionSettingsChange(session.id, "sandboxMode", event.target.value as SandboxMode)
            }
          >
            <option value="workspace-write">workspace-write</option>
            <option value="read-only">read-only</option>
            <option value="danger-full-access">danger-full-access</option>
          </select>
        </div>
        <div className="session-control-group">
          <label className="session-control-label" htmlFor={`approval-policy-${paneId}`}>
            Next prompt approval
          </label>
          <select
            id={`approval-policy-${paneId}`}
            className="session-select"
            value={session.approvalPolicy ?? "never"}
            disabled={isUpdating}
            onChange={(event) =>
              void onSessionSettingsChange(
                session.id,
                "approvalPolicy",
                event.target.value as ApprovalPolicy,
              )
            }
          >
            <option value="never">never</option>
            <option value="on-request">on-request</option>
            <option value="untrusted">untrusted</option>
            <option value="on-failure">on-failure</option>
          </select>
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
          <select
            id={`claude-approval-mode-${paneId}`}
            className="session-select"
            value={session.claudeApprovalMode ?? "ask"}
            disabled={isUpdating}
            onChange={(event) =>
              void onSessionSettingsChange(
                session.id,
                "claudeApprovalMode",
                event.target.value as ClaudeApprovalMode,
              )
            }
          >
            <option value="ask">ask</option>
            <option value="auto-approve">auto-approve</option>
          </select>
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
          <pre className="code-block source-code-block">{fileState.content}</pre>
        </article>
      ) : null}
    </div>
  );
}

function MessageCard({
  message,
  onApprovalDecision,
}: {
  message: Message;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
}) {
  switch (message.type) {
    case "text":
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
}

function PendingPromptCard({
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
  const [expanded, setExpanded] = useState(message.status !== "running");

  return (
    <article className="message-card utility-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="utility-header">
        <div>
          <div className="card-label">Command</div>
          <pre className="command-text">{message.command}</pre>
        </div>
        <div className="utility-actions">
          <span className={`chip chip-status chip-status-${mapCommandStatus(message.status)}`}>
            {message.status}
          </span>
          <button className="ghost-button" type="button" onClick={() => setExpanded((open) => !open)}>
            {expanded ? "Collapse" : "View"}
          </button>
        </div>
      </div>
      {expanded ? <pre className="code-block">{message.output}</pre> : null}
    </article>
  );
}

function DiffCard({ message }: { message: DiffMessage }) {
  const [expanded, setExpanded] = useState(true);

  return (
    <article className="message-card utility-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="utility-header">
        <div>
          <div className="card-label">{message.changeType === "create" ? "New file" : "File edit"}</div>
          <h3>{message.filePath}</h3>
          <p className="support-copy">{message.summary}</p>
        </div>
        <button className="ghost-button" type="button" onClick={() => setExpanded((open) => !open)}>
          {expanded ? "Collapse" : "View"}
        </button>
      </div>
      {expanded ? <pre className="diff-block">{message.diff}</pre> : null}
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
  return (
    <article className="message-card approval-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Approval</div>
      <h3>{message.title}</h3>
      <code className="approval-command">{message.command}</code>
      <p className="support-copy">{message.detail}</p>
      <div className="approval-actions">
        <button
          className="approval-button"
          type="button"
          onClick={() => onApprovalDecision(message.id, "accepted")}
          disabled={message.decision !== "pending"}
        >
          Approve
        </button>
        <button
          className="approval-button"
          type="button"
          onClick={() => onApprovalDecision(message.id, "acceptedForSession")}
          disabled={message.decision !== "pending"}
        >
          Approve for session
        </button>
        <button
          className="approval-button approval-button-reject"
          type="button"
          onClick={() => onApprovalDecision(message.id, "rejected")}
          disabled={message.decision !== "pending"}
        >
          Reject
        </button>
      </div>
      {message.decision !== "pending" ? (
        <p className="approval-result">Decision: {renderDecision(message.decision)}</p>
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

function reconcileWorkspaceState(current: WorkspaceState, sessions: Session[]): WorkspaceState {
  const availableSessionIds = new Set(sessions.map((session) => session.id));
  let panes = current.panes.map((pane) => {
    const sessionIds = pane.sessionIds.filter((sessionId) => availableSessionIds.has(sessionId));
    const activeSessionId = sessionIds.includes(pane.activeSessionId ?? "")
      ? pane.activeSessionId
      : (sessionIds[0] ?? null);

    return {
      ...pane,
      sessionIds,
      activeSessionId,
    };
  });

  let root = pruneWorkspaceNode(current.root, new Set(panes.map((pane) => pane.id)));

  if (!root && panes.length > 0) {
    root = {
      type: "pane",
      paneId: panes[0].id,
    };
  }

  if (!root && sessions.length > 0) {
    const initialPane = createPane(sessions[0].id);
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

  return {
    root,
    panes,
    activePaneId,
  };
}

function openSessionInWorkspaceState(
  workspace: WorkspaceState,
  sessionId: string,
  preferredPaneId: string | null,
): WorkspaceState {
  const existingPane = workspace.panes.find((pane) => pane.sessionIds.includes(sessionId));
  if (existingPane) {
    return activatePane(workspace, existingPane.id, sessionId);
  }

  const targetPaneId = workspace.panes.some((pane) => pane.id === preferredPaneId)
    ? preferredPaneId
    : (workspace.activePaneId ?? workspace.panes[0]?.id ?? null);

  if (!targetPaneId) {
    const pane = createPane(sessionId);
    return {
      root: {
        type: "pane",
        paneId: pane.id,
      },
      panes: [pane],
      activePaneId: pane.id,
    };
  }

  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== targetPaneId) {
        return pane;
      }

      return {
        ...pane,
        sessionIds: pane.sessionIds.includes(sessionId) ? pane.sessionIds : [...pane.sessionIds, sessionId],
        activeSessionId: sessionId,
      };
    }),
    activePaneId: targetPaneId,
  };
}

function activatePane(
  workspace: WorkspaceState,
  paneId: string,
  sessionId?: string | null,
): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      return {
        ...pane,
        activeSessionId:
          sessionId && pane.sessionIds.includes(sessionId)
            ? sessionId
            : (pane.activeSessionId ?? pane.sessionIds[0] ?? null),
      };
    }),
    activePaneId: paneId,
  };
}

function closeSessionTab(
  workspace: WorkspaceState,
  paneId: string,
  sessionId: string,
): WorkspaceState {
  const pane = workspace.panes.find((candidate) => candidate.id === paneId);
  if (!pane) {
    return workspace;
  }

  const nextSessionIds = pane.sessionIds.filter((candidate) => candidate !== sessionId);
  if (nextSessionIds.length === 0) {
    const panes = workspace.panes.filter((candidate) => candidate.id !== paneId);
    const root = removePaneFromTree(workspace.root, paneId);

    return {
      root,
      panes,
      activePaneId:
        panes.some((candidate) => candidate.id === workspace.activePaneId)
          ? workspace.activePaneId
          : (panes[0]?.id ?? null),
    };
  }

  return {
    ...workspace,
    panes: workspace.panes.map((candidate) => {
      if (candidate.id !== paneId) {
        return candidate;
      }

      return {
        ...candidate,
        sessionIds: nextSessionIds,
        activeSessionId:
          candidate.activeSessionId === sessionId ? (nextSessionIds[0] ?? null) : candidate.activeSessionId,
      };
    }),
    activePaneId: paneId,
  };
}

function splitPane(
  workspace: WorkspaceState,
  paneId: string,
  direction: "row" | "column",
): WorkspaceState {
  const pane = workspace.panes.find((candidate) => candidate.id === paneId);
  if (!pane || !workspace.root) {
    return workspace;
  }

  const sessionToMove = pane.sessionIds.length > 1 ? (pane.activeSessionId ?? null) : null;
  const newPane = createPane(sessionToMove ?? undefined, pane.viewMode, pane.sourcePath);
  const panes = workspace.panes.map((candidate) => {
    if (candidate.id !== paneId) {
      return candidate;
    }

    if (!sessionToMove) {
      return candidate;
    }

    const nextSessionIds = candidate.sessionIds.filter((sessionId) => sessionId !== sessionToMove);
    return {
      ...candidate,
      sessionIds: nextSessionIds,
      activeSessionId: nextSessionIds[0] ?? null,
    };
  });

  return {
    root: insertPaneAdjacent(workspace.root, paneId, direction, newPane.id, false),
    panes: [...panes, newPane],
    activePaneId: newPane.id,
  };
}

function placeDraggedSession(
  workspace: WorkspaceState,
  sourcePaneId: string,
  sessionId: string,
  targetPaneId: string,
  placement: TabDropPlacement,
): WorkspaceState {
  const sourcePane = workspace.panes.find((pane) => pane.id === sourcePaneId);
  const targetPane = workspace.panes.find((pane) => pane.id === targetPaneId);
  if (!sourcePane || !targetPane || !sourcePane.sessionIds.includes(sessionId)) {
    return workspace;
  }

  if (placement === "tabs") {
    if (sourcePaneId === targetPaneId) {
      return activatePane(workspace, targetPaneId, sessionId);
    }

    const withoutSource = closeSessionTab(workspace, sourcePaneId, sessionId);
    return addSessionToPane(withoutSource, targetPaneId, sessionId);
  }

  if (sourcePaneId === targetPaneId && sourcePane.sessionIds.length <= 1) {
    return workspace;
  }

  const withoutSource = closeSessionTab(workspace, sourcePaneId, sessionId);
  if (!withoutSource.root || !withoutSource.panes.some((pane) => pane.id === targetPaneId)) {
    return workspace;
  }

  const newPane = createPane(sessionId, targetPane.viewMode, targetPane.sourcePath);
  const direction = placement === "left" || placement === "right" ? "row" : "column";
  const placeBefore = placement === "left" || placement === "top";

  return {
    root: insertPaneAdjacent(withoutSource.root, targetPaneId, direction, newPane.id, placeBefore),
    panes: [...withoutSource.panes, newPane],
    activePaneId: newPane.id,
  };
}

function updateSplitRatio(
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

function createPane(
  sessionId?: string,
  viewMode: PaneViewMode = "session",
  sourcePath: string | null = null,
): WorkspacePane {
  return {
    id: crypto.randomUUID(),
    sessionIds: sessionId ? [sessionId] : [],
    activeSessionId: sessionId ?? null,
    viewMode,
    sourcePath,
  };
}

function setPaneViewMode(
  workspace: WorkspaceState,
  paneId: string,
  viewMode: PaneViewMode,
): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      return {
        ...pane,
        viewMode,
      };
    }),
  };
}

function setPaneSourcePath(
  workspace: WorkspaceState,
  paneId: string,
  sourcePath: string,
): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      return {
        ...pane,
        sourcePath,
      };
    }),
  };
}

function addSessionToPane(
  workspace: WorkspaceState,
  paneId: string,
  sessionId: string,
): WorkspaceState {
  return {
    ...workspace,
    panes: workspace.panes.map((pane) => {
      if (pane.id !== paneId) {
        return pane;
      }

      return {
        ...pane,
        sessionIds: pane.sessionIds.includes(sessionId) ? pane.sessionIds : [...pane.sessionIds, sessionId],
        activeSessionId: sessionId,
      };
    }),
    activePaneId: paneId,
  };
}

function pruneWorkspaceNode(node: WorkspaceNode | null, availablePaneIds: Set<string>): WorkspaceNode | null {
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

function removePaneFromTree(node: WorkspaceNode | null, paneId: string): WorkspaceNode | null {
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
      ratio: 0.5,
      first: placeBefore ? insertedPane : node,
      second: placeBefore ? node : insertedPane,
    };
  }

  return {
    ...node,
    first: insertPaneAdjacent(node.first, paneId, direction, newPaneId, placeBefore),
    second: insertPaneAdjacent(node.second, paneId, direction, newPaneId, placeBefore),
  };
}

function updateSplitRatioInNode(node: WorkspaceNode, splitId: string, ratio: number): WorkspaceNode {
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

function getSplitRatio(node: WorkspaceNode | null, splitId: string): number | null {
  if (!node || node.type === "pane") {
    return null;
  }

  if (node.id === splitId) {
    return node.ratio;
  }

  return getSplitRatio(node.first, splitId) ?? getSplitRatio(node.second, splitId);
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
