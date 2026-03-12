import {
  memo,
  startTransition,
  useDeferredValue,
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
import { createPortal, flushSync } from "react-dom";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  cancelQueuedPrompt,
  createProject,
  createSession,
  fetchFile,
  fetchState,
  killSession,
  pickProjectRoot,
  renameSession,
  saveFile,
  sendMessage,
  stopSession,
  submitApproval,
  type StateResponse,
  updateSessionSettings,
} from "./api";
import { copyTextToClipboard } from "./clipboard";
import { highlightCode } from "./highlight";
import { applyDeltaToSessions } from "./live-updates";
import { resolvePaneScrollCommand } from "./pane-keyboard";
import { AgentSessionPanel, AgentSessionPanelFooter } from "./panels/AgentSessionPanel";
import { DiffPanel } from "./panels/DiffPanel";
import { FileSystemPanel } from "./panels/FileSystemPanel";
import { GitStatusPanel } from "./panels/GitStatusPanel";
import { PaneTabs } from "./panels/PaneTabs";
import { SourcePanel, type SourceFileState } from "./panels/SourcePanel";
import {
  containsSearchMatch,
  highlightReactNodeText,
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";
import {
  buildSessionListSearchResultFromIndex,
  buildSessionSearchIndex,
  buildSessionSearchMatchesFromIndex,
  type SessionListSearchResult,
  type SessionSearchMatch,
} from "./session-find";
import type {
  ApprovalDecision,
  ApprovalMessage,
  ApprovalPolicy,
  AgentType,
  ClaudeApprovalMode,
  CommandMessage,
  CodexState,
  DeltaEvent,
  DiffMessage,
  ImageAttachment,
  MarkdownMessage,
  Message,
  PendingPrompt,
  Project,
  SandboxMode,
  Session,
  TextMessage,
  ThinkingMessage,
} from "./types";
import {
  activatePane,
  closeWorkspaceTab,
  dockControlPanelAtWorkspaceEdge,
  ensureControlPanelInWorkspaceState,
  getSplitRatio,
  openDiffPreviewInWorkspaceState,
  openFilesystemInWorkspaceState,
  openGitStatusInWorkspaceState,
  openSessionInWorkspaceState,
  openSourceInWorkspaceState,
  placeDraggedTab,
  placeExternalTab,
  reconcileWorkspaceState,
  setPaneSourcePath,
  setPaneViewMode,
  splitPane,
  updateSplitRatio,
  type PaneViewMode,
  type SessionPaneViewMode,
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
import type { MonacoAppearance } from "./monaco";
import {
  countSessionsByFilter,
  filterSessionsByListFilter,
  type SessionListFilter,
} from "./session-list-filter";
import { decideDeltaRevisionAction, shouldAdoptStateRevision } from "./state-revision";
import {
  TAB_DRAG_CHANNEL_NAME,
  isWorkspaceTabDragChannelMessage,
  type WorkspaceTabDrag,
  type WorkspaceTabDragChannelMessage,
} from "./tab-drag";

type SessionFlagMap = Record<string, true | undefined>;
type SessionSettingsField = "sandboxMode" | "approvalPolicy" | "claudeApprovalMode";
type SessionSettingsValue = SandboxMode | ApprovalPolicy | ClaudeApprovalMode;
type PreferencesTabId = "themes" | "codex-prompts" | "claude-approvals";
type DraftImageAttachment = ImageAttachment & {
  base64Data: string;
  id: string;
  previewUrl: string;
};

type PendingSessionRename = {
  clientX: number;
  clientY: number;
  sessionId: string;
};

const PENDING_KILL_CLOSE_DELAY_MS = 180;
const PENDING_SESSION_RENAME_CLOSE_DELAY_MS = 300;

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
const ALL_PROJECTS_FILTER_ID = "__all__";

type ComboboxOption = {
  label: string;
  value: string;
};

export default function App() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [codexState, setCodexState] = useState<CodexState>({});
  const [controlPanelSide, setControlPanelSide] = useState<"left" | "right">("left");
  const [workspace, setWorkspace] = useState<WorkspaceState>(() =>
    ensureControlPanelInWorkspaceState({
      root: null,
      panes: [],
      activePaneId: null,
    }),
  );
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
  const [sessionListSearchQuery, setSessionListSearchQuery] = useState("");
  const [selectedProjectId, setSelectedProjectId] = useState<string>(ALL_PROJECTS_FILTER_ID);
  const [newProjectRootPath, setNewProjectRootPath] = useState("");
  const [isCreatingProject, setIsCreatingProject] = useState(false);
  const [themeId, setThemeId] = useState<ThemeId>(() => getStoredThemePreference());
  const [defaultCodexSandboxMode, setDefaultCodexSandboxMode] =
    useState<SandboxMode>("workspace-write");
  const [defaultCodexApprovalPolicy, setDefaultCodexApprovalPolicy] =
    useState<ApprovalPolicy>("never");
  const [defaultClaudeApprovalMode, setDefaultClaudeApprovalMode] =
    useState<ClaudeApprovalMode>("ask");
  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState<PreferencesTabId>("themes");
  const [pendingSessionRename, setPendingSessionRename] = useState<PendingSessionRename | null>(
    null,
  );
  const [pendingSessionRenameDraft, setPendingSessionRenameDraft] = useState("");
  const [pendingSessionRenameStyle, setPendingSessionRenameStyle] = useState<CSSProperties | null>(
    null,
  );
  const [pendingKillPopoverStyle, setPendingKillPopoverStyle] = useState<CSSProperties | null>(
    null,
  );
  const [pendingScrollToBottomRequest, setPendingScrollToBottomRequest] = useState<{
    sessionId: string;
    token: number;
  } | null>(null);
  const [windowId] = useState(() => crypto.randomUUID());
  const [draggedTab, setDraggedTab] = useState<WorkspaceTabDrag | null>(null);
  const [externalDraggedTab, setExternalDraggedTab] = useState<WorkspaceTabDrag | null>(null);
  const resizeStateRef = useRef<{
    splitId: string;
    direction: "row" | "column";
    startRatio: number;
    startX: number;
    startY: number;
    size: number;
  } | null>(null);
  const draftAttachmentsRef = useRef<Record<string, DraftImageAttachment[]>>({});
  const dragChannelRef = useRef<BroadcastChannel | null>(null);
  const draggedTabRef = useRef<WorkspaceTabDrag | null>(null);
  const sessionListSearchInputRef = useRef<HTMLInputElement>(null);
  const pendingSessionRenameTriggerRef = useRef<HTMLElement | null>(null);
  const pendingSessionRenamePopoverRef = useRef<HTMLFormElement | null>(null);
  const pendingSessionRenameInputRef = useRef<HTMLInputElement | null>(null);
  const pendingSessionRenameCloseTimeoutRef = useRef<number | null>(null);
  const pendingKillTriggerRef = useRef<HTMLButtonElement | null>(null);
  const pendingKillPopoverRef = useRef<HTMLDivElement | null>(null);
  const pendingKillConfirmButtonRef = useRef<HTMLButtonElement | null>(null);
  const pendingKillCloseTimeoutRef = useRef<number | null>(null);
  const isMountedRef = useRef(true);
  const sessionsRef = useRef<Session[]>([]);
  const latestStateRevisionRef = useRef<number | null>(null);
  const stateResyncInFlightRef = useRef(false);
  const stateResyncPendingRef = useRef(false);
  const paneShouldStickToBottomRef = useRef<Record<string, boolean | undefined>>({});
  const paneScrollPositionsRef = useRef<
    Record<string, Record<string, { top: number; shouldStick: boolean }>>
  >({});
  const paneContentSignaturesRef = useRef<Record<string, Record<string, string>>>({});

  const projectLookup = useMemo(
    () => new Map(projects.map((project) => [project.id, project])),
    [projects],
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
    workspace.panes.find((pane) => pane.id === workspace.activePaneId) ?? workspace.panes[0] ?? null;
  const activeSession = activePane?.activeSessionId
    ? (sessionLookup.get(activePane.activeSessionId) ?? null)
    : null;
  const openSessionIds = useMemo(
    () =>
      new Set(
        workspace.panes.flatMap((pane) =>
          pane.tabs.flatMap((tab) => (tab.kind === "session" ? [tab.sessionId] : [])),
        ),
      ),
    [workspace.panes],
  );
  const workspaceHasOnlyControlPanel = useMemo(
    () =>
      workspace.panes.length === 1 &&
      workspace.panes[0]?.tabs.length === 1 &&
      workspace.panes[0]?.tabs[0]?.kind === "controlPanel",
    [workspace.panes],
  );
  const selectedProject =
    selectedProjectId === ALL_PROJECTS_FILTER_ID
      ? null
      : (projectLookup.get(selectedProjectId) ?? null);
  const projectScopedSessions = useMemo(() => {
    if (!selectedProject) {
      return sessions;
    }

    return sessions.filter((session) => session.projectId === selectedProject.id);
  }, [selectedProject, sessions]);
  const sessionFilterCounts = useMemo(
    () => countSessionsByFilter(projectScopedSessions),
    [projectScopedSessions],
  );
  const statusFilteredSessions = useMemo(() => {
    return filterSessionsByListFilter(projectScopedSessions, sessionListFilter);
  }, [projectScopedSessions, sessionListFilter]);
  const sessionListSearchIndex = useMemo(
    () =>
      new Map(
        projectScopedSessions.map((session) => [session.id, buildSessionSearchIndex(session)] as const),
      ),
    [projectScopedSessions],
  );
  const trimmedSessionListSearchQuery = sessionListSearchQuery.trim();
  const deferredSessionListSearchQuery = useDeferredValue(trimmedSessionListSearchQuery);
  const effectiveSessionListSearchQuery =
    trimmedSessionListSearchQuery.length === 0 ? "" : deferredSessionListSearchQuery;
  const hasSessionListSearch = effectiveSessionListSearchQuery.length > 0;
  const sessionListSearchResults = useMemo(() => {
    if (!hasSessionListSearch) {
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

    return statusFilteredSessions.filter((session) => sessionListSearchResults.has(session.id));
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
  const editorAppearance: MonacoAppearance = isHexColorDark(activeTheme.swatches[0]) ? "dark" : "light";
  const activeDraggedTab = draggedTab ?? externalDraggedTab;

  function focusSessionListSearch(selectAll = false) {
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
    return dockControlPanelAtWorkspaceEdge(
      ensureControlPanelInWorkspaceState(nextWorkspace),
      side,
    );
  }

  function adoptSessions(
    nextSessions: Session[],
    options?: { openSessionId?: string; paneId?: string | null },
  ) {
    const mergedSessions = reconcileSessions(sessionsRef.current, nextSessions);
    const availableSessionIds = new Set(mergedSessions.map((session) => session.id));

    sessionsRef.current = mergedSessions;
    setSessions(mergedSessions);
    setWorkspace((current) => {
      const reconciled = applyControlPanelLayout(reconcileWorkspaceState(current, mergedSessions));
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
    setPendingSessionRename((current) =>
      current && availableSessionIds.has(current.sessionId) ? current : null,
    );
    setUpdatingSessionIds((current) => pruneSessionFlags(current, availableSessionIds));
  }

  function adoptState(
    nextState: StateResponse,
    options?: { openSessionId?: string; paneId?: string | null },
  ) {
    if (!shouldAdoptStateRevision(latestStateRevisionRef.current, nextState.revision)) {
      return false;
    }

    latestStateRevisionRef.current = nextState.revision;
    setCodexState(nextState.codex ?? {});
    setProjects(nextState.projects ?? []);
    adoptSessions(nextState.sessions, options);
    if (options?.openSessionId) {
      const openedSession = nextState.sessions.find((session) => session.id === options.openSessionId);
      setSelectedProjectId(openedSession?.projectId ?? ALL_PROJECTS_FILTER_ID);
    }
    return true;
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

            try {
              const state = await fetchState();
              if (cancelled) {
                return;
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
        const revisionAction = decideDeltaRevisionAction(
          latestStateRevisionRef.current,
          delta.revision,
        );
        if (revisionAction === "ignore") {
          setRequestError(null);
          return;
        }
        if (revisionAction === "resync") {
          requestStateResync();
          return;
        }

        const result = applyDeltaToSessions(sessionsRef.current, delta);
        if (result.kind === "applied") {
          latestStateRevisionRef.current = delta.revision;
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
    eventSource.onerror = () => {
      if (!cancelled && latestStateRevisionRef.current === null) {
        requestStateResync();
      }
    };

    return () => {
      cancelled = true;
      eventSource.removeEventListener("state", handleStateEvent as EventListener);
      eventSource.removeEventListener("delta", handleDeltaEvent as EventListener);
      eventSource.close();
    };
  }, []);

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
          setDraggedTab((current) => (current?.dragId === message.dragId ? null : current));
          setWorkspace((current) =>
            applyControlPanelLayout(closeWorkspaceTab(current, message.sourcePaneId, message.tabId)),
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
    };
  }, []);

  useEffect(() => {
    return () => {
      releaseDraftAttachments(Object.values(draftAttachmentsRef.current).flat());
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
      const preferredLeft = triggerRect.left + triggerRect.width / 2 - popoverRect.width / 2;
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

    const frameId = window.requestAnimationFrame(updatePendingSessionRenameStyle);
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

  async function handleNewSession(preferredPaneId: string | null = null) {
    setIsCreating(true);
    try {
      const targetPaneId = preferredPaneId ?? workspace.activePaneId;
      const targetProjectId =
        selectedProjectId === ALL_PROJECTS_FILTER_ID
          ? (activeSession?.projectId ?? projects[0]?.id ?? null)
          : selectedProjectId;
      const created = await createSession({
        agent: newSessionAgent,
        approvalPolicy:
          newSessionAgent === "Codex" ? defaultCodexApprovalPolicy : undefined,
        claudeApprovalMode:
          newSessionAgent === "Claude" ? defaultClaudeApprovalMode : undefined,
        sandboxMode: newSessionAgent === "Codex" ? defaultCodexSandboxMode : undefined,
        projectId: targetProjectId ?? undefined,
        workdir: targetProjectId ? undefined : activeSession?.workdir,
      });
      const adopted = adoptState(created.state, {
        openSessionId: created.sessionId,
        paneId: targetPaneId,
      });
      if (!adopted) {
        setWorkspace((current) =>
          applyControlPanelLayout(openSessionInWorkspaceState(current, created.sessionId, targetPaneId)),
        );
      }
      setRequestError(null);
    } catch (error) {
      setRequestError(getErrorMessage(error));
    } finally {
      setIsCreating(false);
    }
  }

  async function handleCreateProject() {
    const rootPath = newProjectRootPath.trim();
    if (!rootPath) {
      setRequestError("Enter a project root path.");
      return;
    }

    setIsCreatingProject(true);
    try {
      const created = await createProject({ rootPath });
      adoptState(created.state);
      setSelectedProjectId(created.projectId);
      setNewProjectRootPath("");
      setRequestError(null);
    } catch (error) {
      setRequestError(getErrorMessage(error));
    } finally {
      setIsCreatingProject(false);
    }
  }

  async function handlePickProjectRoot() {
    setIsCreatingProject(true);
    try {
      const response = await pickProjectRoot();
      if (response.path) {
        setNewProjectRootPath(response.path);
        setRequestError(null);
      }
    } catch (error) {
      setRequestError(getErrorMessage(error));
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

  function handleKillSession(sessionId: string, trigger?: HTMLButtonElement | null) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return;
    }

    closePendingSessionRename();
    clearPendingKillCloseTimeout();
    pendingKillTriggerRef.current = trigger ?? null;
    setPendingKillSessionId((current) => (current === sessionId ? null : sessionId));
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
      setPendingKillSessionId((current) => (current === sessionId ? null : current));
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

    setUpdatingSessionIds((current) => setSessionFlag(current, session.id, true));
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

      setRequestError(getErrorMessage(error));
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) => setSessionFlag(current, session.id, false));
      }
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

  function handleSidebarSessionClick(sessionId: string, preferredPaneId: string | null = null) {
    const session = sessionLookup.get(sessionId);
    closePendingSessionRename();
    setKillRevealSessionId(null);
    setSelectedProjectId(session?.projectId ?? ALL_PROJECTS_FILTER_ID);
    requestScrollToBottom(sessionId);
    setWorkspace((current) =>
      applyControlPanelLayout(
        openSessionInWorkspaceState(current, sessionId, preferredPaneId ?? current.activePaneId),
      ),
    );
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

    setWorkspace((current) => activatePane(current, paneId, tabId));
  }

  function handleCloseTab(paneId: string, tabId: string) {
    setWorkspace((current) => applyControlPanelLayout(closeWorkspaceTab(current, paneId, tabId)));
  }

  function handleSplitPane(paneId: string, direction: "row" | "column") {
    setWorkspace((current) => applyControlPanelLayout(splitPane(current, paneId, direction)));
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

  function handleTabDrop(targetPaneId: string, placement: TabDropPlacement, tabIndex?: number) {
    if (draggedTab) {
      const drop = draggedTab;
      draggedTabRef.current = null;
      setDraggedTab(null);
      const nextControlPanelSide =
        drop.tab.kind === "controlPanel" && (placement === "left" || placement === "right")
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

    if (!externalDraggedTab) {
      return;
    }

    const drop = externalDraggedTab;
    setExternalDraggedTab((current) => (current?.dragId === drop.dragId ? null : current));
    const nextControlPanelSide =
      drop.tab.kind === "controlPanel" && (placement === "left" || placement === "right")
        ? placement
        : controlPanelSide;
    if (nextControlPanelSide !== controlPanelSide) {
      setControlPanelSide(nextControlPanelSide);
    }
    // Only ask the source window to remove its tab after this window has applied the drop.
    flushSync(() => {
      setWorkspace((current) =>
        applyControlPanelLayout(
          placeExternalTab(current, drop.tab, targetPaneId, placement, tabIndex),
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

  function handlePaneViewModeChange(paneId: string, viewMode: SessionPaneViewMode) {
    if (viewMode === "session") {
      const pane = paneLookup.get(paneId);
      const activeTab = pane?.tabs.find((candidate) => candidate.id === pane.activeTabId);
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

  function handleOpenSourceTab(paneId: string, path: string | null, originSessionId: string | null) {
    setWorkspace((current) =>
      applyControlPanelLayout(openSourceInWorkspaceState(current, path, paneId, originSessionId)),
    );
  }

  function handleOpenDiffPreviewTab(
    paneId: string,
    message: DiffMessage,
    originSessionId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openDiffPreviewInWorkspaceState(
          current,
          {
            changeType: message.changeType,
            diff: message.diff,
            diffMessageId: message.id,
            filePath: message.filePath,
            language: message.language ?? null,
            originSessionId,
            summary: message.summary,
          },
          paneId,
        ),
      ),
    );
  }

  function handleOpenFilesystemTab(
    paneId: string,
    rootPath: string | null,
    originSessionId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openFilesystemInWorkspaceState(current, rootPath, paneId, originSessionId),
      ),
    );
  }

  function handleOpenGitStatusTab(
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
  ) {
    setWorkspace((current) =>
      applyControlPanelLayout(
        openGitStatusInWorkspaceState(current, workdir, paneId, originSessionId),
      ),
    );
  }

  function renderWorkspaceControlSurface(paneId: string): JSX.Element {
    const surfaceId = paneId;
    const newSessionAgentId = `new-session-agent-${surfaceId}`;
    const newProjectRootId = `new-project-root-${surfaceId}`;

    const content = (
      <>
        <div className="brand-block">
          <h1>TermAl</h1>
        </div>

        <div className="new-session-controls">
          <label className="session-control-label" htmlFor={newSessionAgentId}>
            New session
          </label>
          <p className="session-control-hint">
            {selectedProject
              ? `Creates in ${selectedProject.name}.`
              : "Creates in the selected project or the active session workspace."}
          </p>
          <div className="new-session-row">
            <ThemedCombobox
              id={newSessionAgentId}
              className="new-session-agent-select"
              value={newSessionAgent}
              options={NEW_SESSION_AGENT_OPTIONS as readonly ComboboxOption[]}
              onChange={(nextValue) => setNewSessionAgent(nextValue as AgentType)}
              disabled={isCreating}
            />
            <button
              className="new-session-button"
              type="button"
              onClick={() => void handleNewSession(paneId)}
              disabled={isCreating}
            >
              {isCreating ? "Creating..." : "New Session"}
            </button>
          </div>
        </div>

        <section className="project-controls" aria-label="Projects">
          <div className="project-controls-header">
            <label className="session-control-label" htmlFor={newProjectRootId}>
              Projects
            </label>
            <span className="project-count-badge">{projects.length}</span>
          </div>
          <div className="project-create-row">
            <input
              id={newProjectRootId}
              className="themed-input project-root-input"
              type="text"
              value={newProjectRootPath}
              placeholder="/path/to/project"
              onChange={(event) => setNewProjectRootPath(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  void handleCreateProject();
                }
              }}
              disabled={isCreatingProject}
            />
            <button
              className="ghost-button project-create-button"
              type="button"
              onClick={() => void handlePickProjectRoot()}
              disabled={isCreatingProject}
            >
              Choose Folder
            </button>
            <button
              className="ghost-button project-create-button"
              type="button"
              onClick={() => void handleCreateProject()}
              disabled={isCreatingProject}
            >
              {isCreatingProject ? "Adding..." : "Add"}
            </button>
          </div>
          <div className="project-list" role="list">
            <button
              className={`project-row ${selectedProjectId === ALL_PROJECTS_FILTER_ID ? "selected" : ""}`}
              type="button"
              onClick={() => setSelectedProjectId(ALL_PROJECTS_FILTER_ID)}
            >
              <span className="project-row-copy">
                <strong>All projects</strong>
                <span className="project-row-path">Show every session in this window.</span>
              </span>
              <span className="project-row-count">{sessions.length}</span>
            </button>
            {projects.map((project) => {
              const isSelected = project.id === selectedProjectId;

              return (
                <button
                  key={project.id}
                  className={`project-row ${isSelected ? "selected" : ""}`}
                  type="button"
                  onClick={() => setSelectedProjectId(project.id)}
                >
                  <span className="project-row-copy">
                    <strong>{project.name}</strong>
                    <span className="project-row-path">{project.rootPath}</span>
                  </span>
                  <span className="project-row-count">{projectSessionCounts.get(project.id) ?? 0}</span>
                </button>
              );
            })}
          </div>
        </section>

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
          <div className="session-list-header">
            <span className="session-control-label">Sessions</span>
            <span className="session-list-scope">
              {selectedProject ? selectedProject.name : "All projects"}
            </span>
          </div>
          <div className="session-list-tools">
            <input
              ref={sessionListSearchInputRef}
              className="themed-input session-list-search-input"
              type="search"
              value={sessionListSearchQuery}
              placeholder="Search sessions"
              spellCheck={false}
              aria-label="Search sessions"
              title={`Search across visible sessions (${primaryModifierLabel()}+Shift+F)`}
              onChange={(event) => setSessionListSearchQuery(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.preventDefault();
                  if (sessionListSearchQuery) {
                    setSessionListSearchQuery("");
                  } else {
                    event.currentTarget.blur();
                  }
                }
              }}
            />
            {hasSessionListSearch ? (
              <div className="session-list-search-meta" aria-live="polite">
                {filteredSessions.length === 1
                  ? "1 matching session"
                  : `${filteredSessions.length} matching sessions`}
              </div>
            ) : null}
          </div>
          {filteredSessions.length > 0 ? (
            filteredSessions.map((session) => {
              const isActive = session.id === activeSession?.id;
              const isOpen = openSessionIds.has(session.id);
              const isKilling = Boolean(killingSessionIds[session.id]);
              const isKillConfirmationOpen = pendingKillSessionId === session.id;
              const isKillVisible =
                isKilling || isKillConfirmationOpen || killRevealSessionId === session.id;
              const searchResult = sessionListSearchResults.get(session.id);

              return (
                <div
                  key={`${surfaceId}-${session.id}`}
                  className={`session-row-shell ${isActive ? "selected" : ""} ${isOpen ? "open" : ""} ${isKillVisible ? "kill-armed" : ""}`}
                  onMouseLeave={() => {
                    if (!isKilling && !isKillConfirmationOpen) {
                      setKillRevealSessionId((current) => (current === session.id ? null : current));
                    }
                  }}
                  onBlur={(event) => {
                    const nextTarget = event.relatedTarget;
                    if (
                      !isKilling &&
                      !isKillConfirmationOpen &&
                      (!(nextTarget instanceof Node) || !event.currentTarget.contains(nextTarget))
                    ) {
                      setKillRevealSessionId((current) => (current === session.id ? null : current));
                    }
                  }}
                >
                  <button
                    className={`session-row ${isActive ? "selected" : ""} ${isOpen ? "open" : ""}`}
                    type="button"
                    onClick={() => handleSidebarSessionClick(session.id, paneId)}
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
                    <div className="session-avatar">{session.emoji}</div>
                    <div className="session-copy">
                      <div className="session-title-line">
                        <strong>{session.name}</strong>
                        {searchResult ? (
                          <span className="session-search-count">
                            {searchResult.matchCount} hit{searchResult.matchCount === 1 ? "" : "s"}
                          </span>
                        ) : null}
                      </div>
                      <div className="session-meta">
                        {session.agent} <span className="meta-separator">/</span> {session.workdir}
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
                    onClick={(event) => {
                      handleKillSession(session.id, event.currentTarget);
                    }}
                    disabled={isKilling}
                    aria-expanded={isKillConfirmationOpen}
                    aria-controls={isKillConfirmationOpen ? `kill-session-popover-${session.id}` : undefined}
                    aria-label={`Kill ${session.name}`}
                  >
                    {isKilling ? "Killing" : "Kill"}
                  </button>
                </div>
              );
            })
          ) : (
            <div className="session-filter-empty">
              {sessions.length === 0
                ? "No sessions yet."
                : hasSessionListSearch
                  ? selectedProject
                    ? `No sessions match this search in ${selectedProject.name}.`
                    : "No sessions match this search."
                  : selectedProject
                    ? `No ${sessionListFilter === "all" ? "" : `${sessionListFilter} `}sessions in ${selectedProject.name}.`
                    : "No sessions match this filter."}
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
      </>
    );
    return <div className="sidebar sidebar-panel">{content}</div>;
  }

  return (
    <div className="shell">
      <div className="background-orbit background-orbit-left" />
      <div className="background-orbit background-orbit-right" />

      <main className="workspace-shell">
        {requestError ? (
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
              windowId={windowId}
              draggedTab={activeDraggedTab}
              editorAppearance={editorAppearance}
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
              onOpenFilesystemTab={handleOpenFilesystemTab}
              onOpenGitStatusTab={handleOpenGitStatusTab}
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
              onRenameSessionRequest={handleSessionRenameRequest}
              onScrollToBottomRequestHandled={handleScrollToBottomRequestHandled}
              onSessionSettingsChange={handleSessionSettingsChange}
              renderControlPanel={(paneId) => renderWorkspaceControlSurface(paneId)}
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
                    className="ghost-button session-rename-cancel"
                    type="button"
                    onClick={() => {
                      closePendingSessionRename(true);
                    }}
                  >
                    Cancel
                  </button>
                  <button
                    className="send-button session-rename-save"
                    type="submit"
                    disabled={!pendingSessionRenameValue || isPendingSessionRenameSubmitting}
                  >
                    {isPendingSessionRenameSubmitting ? "Saving" : "Save"}
                  </button>
                </div>
              </form>
            </>,
            document.body,
          )
        : null}
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
  windowId,
  draggedTab,
  editorAppearance,
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
  onOpenFilesystemTab,
  onOpenGitStatusTab,
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
  onRenameSessionRequest,
  onScrollToBottomRequestHandled,
  onSessionSettingsChange,
  renderControlPanel,
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
  windowId: string;
  draggedTab: WorkspaceTabDrag | null;
  editorAppearance: MonacoAppearance;
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
  onTabDrop: (targetPaneId: string, placement: TabDropPlacement, tabIndex?: number) => void;
  onPaneViewModeChange: (paneId: string, viewMode: SessionPaneViewMode) => void;
  onOpenSourceTab: (paneId: string, path: string | null, originSessionId: string | null) => void;
  onOpenDiffPreviewTab: (
    paneId: string,
    message: DiffMessage,
    originSessionId: string | null,
  ) => void;
  onOpenFilesystemTab: (
    paneId: string,
    rootPath: string | null,
    originSessionId: string | null,
  ) => void;
  onOpenGitStatusTab: (
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
  ) => void;
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
  renderControlPanel: (paneId: string) => JSX.Element;
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
        sessionLookup={sessionLookup}
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
        windowId={windowId}
        draggedTab={draggedTab}
        editorAppearance={editorAppearance}
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
        onOpenFilesystemTab={onOpenFilesystemTab}
        onOpenGitStatusTab={onOpenGitStatusTab}
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
        onRenameSessionRequest={onRenameSessionRequest}
        onScrollToBottomRequestHandled={onScrollToBottomRequestHandled}
        onSessionSettingsChange={onSessionSettingsChange}
        renderControlPanel={renderControlPanel}
      />
    );
  }

  const firstContainsControlPanel = workspaceNodeContainsControlPanel(node.first, paneLookup);
  const secondContainsControlPanel = workspaceNodeContainsControlPanel(node.second, paneLookup);
  const shouldPinControlPanelBranch =
    node.direction === "row" && firstContainsControlPanel !== secondContainsControlPanel;
  const firstBranchClassName = shouldPinControlPanelBranch
    ? `tile-branch ${firstContainsControlPanel ? "control-panel-branch" : "flexible-branch"}`
    : "tile-branch";
  const secondBranchClassName = shouldPinControlPanelBranch
    ? `tile-branch ${secondContainsControlPanel ? "control-panel-branch" : "flexible-branch"}`
    : "tile-branch";

  return (
    <div className={`tile-split tile-split-${node.direction}`}>
      <div
        className={firstBranchClassName}
        style={shouldPinControlPanelBranch ? undefined : { flexGrow: node.ratio, flexBasis: 0 }}
      >
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
          windowId={windowId}
          draggedTab={draggedTab}
          editorAppearance={editorAppearance}
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
          onOpenFilesystemTab={onOpenFilesystemTab}
          onOpenGitStatusTab={onOpenGitStatusTab}
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
          onRenameSessionRequest={onRenameSessionRequest}
          onScrollToBottomRequestHandled={onScrollToBottomRequestHandled}
          onSessionSettingsChange={onSessionSettingsChange}
          renderControlPanel={renderControlPanel}
        />
      </div>

      <div
        className={`tile-divider tile-divider-${node.direction}${shouldPinControlPanelBranch ? " fixed" : ""}`}
        onPointerDown={
          shouldPinControlPanelBranch
            ? undefined
            : (event) => onResizeStart(node.id, node.direction, event)
        }
      />

      <div
        className={secondBranchClassName}
        style={shouldPinControlPanelBranch ? undefined : { flexGrow: 1 - node.ratio, flexBasis: 0 }}
      >
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
          windowId={windowId}
          draggedTab={draggedTab}
          editorAppearance={editorAppearance}
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
          onOpenFilesystemTab={onOpenFilesystemTab}
          onOpenGitStatusTab={onOpenGitStatusTab}
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
          onRenameSessionRequest={onRenameSessionRequest}
          onScrollToBottomRequestHandled={onScrollToBottomRequestHandled}
          onSessionSettingsChange={onSessionSettingsChange}
          renderControlPanel={renderControlPanel}
        />
      </div>
    </div>
  );
}

function SessionPaneView({
  pane,
  codexState,
  sessionLookup,
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
  windowId,
  draggedTab,
  editorAppearance,
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
  onOpenFilesystemTab,
  onOpenGitStatusTab,
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
  onRenameSessionRequest,
  onScrollToBottomRequestHandled,
  onSessionSettingsChange,
  renderControlPanel,
}: {
  pane: WorkspacePane;
  codexState: CodexState;
  sessionLookup: Map<string, Session>;
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
  windowId: string;
  draggedTab: WorkspaceTabDrag | null;
  editorAppearance: MonacoAppearance;
  onActivatePane: (paneId: string) => void;
  onSelectTab: (paneId: string, tabId: string) => void;
  onCloseTab: (paneId: string, tabId: string) => void;
  onSplitPane: (paneId: string, direction: "row" | "column") => void;
  onTabDragStart: (drag: WorkspaceTabDrag) => void;
  onTabDragEnd: () => void;
  onTabDrop: (targetPaneId: string, placement: TabDropPlacement, tabIndex?: number) => void;
  onPaneViewModeChange: (paneId: string, viewMode: SessionPaneViewMode) => void;
  onOpenSourceTab: (paneId: string, path: string | null, originSessionId: string | null) => void;
  onOpenDiffPreviewTab: (
    paneId: string,
    message: DiffMessage,
    originSessionId: string | null,
  ) => void;
  onOpenFilesystemTab: (
    paneId: string,
    rootPath: string | null,
    originSessionId: string | null,
  ) => void;
  onOpenGitStatusTab: (
    paneId: string,
    workdir: string | null,
    originSessionId: string | null,
  ) => void;
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
  renderControlPanel: (paneId: string) => JSX.Element;
}) {
  const activeTab = pane.tabs.find((tab) => tab.id === pane.activeTabId) ?? pane.tabs[0] ?? null;
  const activeControlPanelTab = activeTab?.kind === "controlPanel" ? activeTab : null;
  const activeSourceTab = activeTab?.kind === "source" ? activeTab : null;
  const activeFilesystemTab = activeTab?.kind === "filesystem" ? activeTab : null;
  const activeGitStatusTab = activeTab?.kind === "gitStatus" ? activeTab : null;
  const activeDiffPreviewTab = activeTab?.kind === "diffPreview" ? activeTab : null;
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
  const sessions = useMemo(() => sessionTabs.map(({ session }) => session), [sessionTabs]);
  const [sourceDraft, setSourceDraft] = useState(pane.sourcePath ?? "");
  const [fileState, setFileState] = useState<SourceFileState>({
    status: "idle",
    path: "",
    content: "",
    error: null,
    language: null,
  });
  const messageStackRef = useRef<HTMLElement | null>(null);
  const [activeDropPlacement, setActiveDropPlacement] = useState<Exclude<TabDropPlacement, "tabs"> | null>(null);
  const [visitedSessionIds, setVisitedSessionIds] = useState<Record<string, true | undefined>>({});
  const [cachedSessionOrder, setCachedSessionOrder] = useState<string[]>([]);
  const [newResponseIndicatorByKey, setNewResponseIndicatorByKey] = useState<
    Record<string, true | undefined>
  >({});
  const [isSessionFindOpen, setIsSessionFindOpen] = useState(false);
  const [sessionFindQuery, setSessionFindQuery] = useState("");
  const [sessionFindActiveIndex, setSessionFindActiveIndex] = useState(0);
  const sessionFindInputRef = useRef<HTMLInputElement>(null);
  const sessionSearchItemRefsRef = useRef<Record<string, HTMLElement | null>>({});
  const paneHasControlPanel = useMemo(
    () => pane.tabs.some((tab) => tab.kind === "controlPanel"),
    [pane.tabs],
  );
  const allowedDropPlacements = useMemo<Exclude<TabDropPlacement, "tabs">[]>(
    () =>
      draggedTab && (draggedTab.tab.kind === "controlPanel" || paneHasControlPanel)
        ? ["left", "right"]
        : ["left", "top", "right", "bottom"],
    [draggedTab, paneHasControlPanel],
  );
  const showDropOverlay = Boolean(draggedTab) && !(
    draggedTab?.sourceWindowId === windowId &&
    draggedTab?.sourcePaneId === pane.id &&
    pane.tabs.length <= 1
  );
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
    isSessionTabActive &&
    pane.viewMode === "session" &&
    Boolean(activeSession) &&
    (activeSession?.status === "active" || (!isSessionBusy && isSending));
  const canFindInSession =
    isSessionTabActive &&
    pane.viewMode === "session" &&
    Boolean(activeSession);
  const activeSessionFindSearchIndex = useMemo(
    () => (activeSession ? buildSessionSearchIndex(activeSession) : null),
    [activeSession],
  );
  const sessionSearchMatches = useMemo(
    () =>
      canFindInSession && activeSessionFindSearchIndex
        ? buildSessionSearchMatchesFromIndex(activeSessionFindSearchIndex, sessionFindQuery)
        : [],
    [activeSessionFindSearchIndex, canFindInSession, sessionFindQuery],
  );
  const hasSessionFindQuery = sessionFindQuery.trim().length > 0;
  const sessionSearchMatchedItemKeys = useMemo(
    () => new Set(sessionSearchMatches.map((match) => match.itemKey)),
    [sessionSearchMatches],
  );
  const activeSessionSearchMatch =
    sessionSearchMatches.length > 0
      ? sessionSearchMatches[Math.min(sessionFindActiveIndex, sessionSearchMatches.length - 1)] ?? null
      : null;
  const activeSessionSearchMatchIndex = activeSessionSearchMatch
    ? Math.min(sessionFindActiveIndex, sessionSearchMatches.length - 1)
    : -1;
  const waitingIndicatorPrompt =
    !isSessionBusy && isSending ? null : lastUserPrompt;
  const composerInputDisabled = !activeSession || isStopping;
  const composerSendDisabled = !activeSession || isSending || isStopping;
  const scrollStateKey = activeSourceTab
    ? `${pane.id}:source:${activeSourceTab.path ?? "empty"}`
    : activeFilesystemTab
      ? `${pane.id}:filesystem:${activeFilesystemTab.rootPath ?? "empty"}`
      : activeGitStatusTab
        ? `${pane.id}:gitStatus:${activeGitStatusTab.workdir ?? "empty"}`
        : activeDiffPreviewTab
          ? `${pane.id}:diffPreview:${activeDiffPreviewTab.diffMessageId}`
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

  async function handleSourceFileSave(path: string, content: string) {
    const response = await saveFile(path, content);
    setFileState({
      status: "ready",
      path: response.path,
      content: response.content,
      error: null,
      language: response.language ?? null,
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

  function handleConversationSearchItemMount(itemKey: string, node: HTMLElement | null) {
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
      return (safeCurrent + direction + sessionSearchMatches.length) % sessionSearchMatches.length;
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
      scrollMessageStackToBoundary(command.direction === "up" ? "top" : "bottom");
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
    if (!hasSessionFindQuery || !activeSessionSearchMatch) {
      return;
    }

    const node = sessionSearchItemRefsRef.current[activeSessionSearchMatch.itemKey];
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
  }, [activeSessionSearchMatch, hasSessionFindQuery, paneScrollPositions, scrollStateKey]);

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
  }, [activeSession, isSessionTabActive, pane.viewMode, scrollStateKey, visitedSessionIds]);

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
    if (!activeSession || !isSessionTabActive) {
      return;
    }

    const previousSignature = paneContentSignatures[scrollStateKey];
    paneContentSignatures[scrollStateKey] = visibleContentSignature;
    if (previousSignature === undefined || previousSignature === visibleContentSignature) {
      return;
    }

    if (hasSessionFindQuery) {
      setShouldStickToBottom(false);
      if (pane.viewMode === "session" && visibleLastMessageAuthor === "assistant") {
        setNewResponseIndicator(scrollStateKey, true);
      }
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
    setSourceDraft(pane.sourcePath ?? "");
  }, [pane.id, pane.sourcePath]);

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

  return (
    <section
      className={`workspace-pane thread panel ${isActive ? "active" : ""}`}
      onMouseDown={() => onActivatePane(pane.id)}
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
            <PaneTabs
              paneId={pane.id}
              windowId={windowId}
              tabs={pane.tabs}
              activeTabId={activeTab?.id ?? null}
              codexState={codexState}
              sessionLookup={sessionLookup}
              draggedTab={draggedTab}
              onSelectTab={onSelectTab}
              onCloseTab={onCloseTab}
              onTabDragStart={onTabDragStart}
              onTabDragEnd={onTabDragEnd}
              onTabDrop={onTabDrop}
              onRenameSessionRequest={onRenameSessionRequest}
            />
          </div>

        </div>

        <div className="pane-view-strip">
          {activeTab?.kind === "session" ? (
            <div className="pane-view-strip-left">
              {(["session", "prompt", "commands", "diffs"] as SessionPaneViewMode[]).map((viewMode) => (
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
                onClick={() =>
                  onOpenSourceTab(pane.id, candidateSourcePaths[0] ?? null, activeSession?.id ?? null)
                }
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
                  )
                }
                >
                  Git
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
          ) : activeTab?.kind === "controlPanel" ? null : (
            <div className="pane-view-strip-left">
              <span className="chip">
                {activeTab?.kind === "source"
                  ? "File viewer"
                  : activeTab?.kind === "filesystem"
                    ? "File browser"
                    : activeTab?.kind === "gitStatus"
                      ? "Git status"
                      : activeTab?.kind === "controlPanel"
                        ? "Control panel"
                      : activeTab?.kind === "diffPreview"
                        ? "Diff preview"
                      : "Panel"}
              </span>
            </div>
          )}
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
            {activeSession && !activeControlPanelTab ? (
              <div className="thread-chips">
                <span className="chip">{activeSession.workdir}</span>
                {activeSourceTab?.path ? <span className="chip">{activeSourceTab.path}</span> : null}
                {activeFilesystemTab?.rootPath ? (
                  <span className="chip">{activeFilesystemTab.rootPath}</span>
                ) : null}
                {activeGitStatusTab?.workdir ? (
                  <span className="chip">{activeGitStatusTab.workdir}</span>
                ) : null}
                {activeDiffPreviewTab?.filePath ? (
                  <span className="chip">{activeDiffPreviewTab.filePath}</span>
                ) : null}
              </div>
            ) : null}
          </div>
        </div>
      </div>

      <section
        ref={messageStackRef}
        className={`message-stack${activeControlPanelTab ? " control-panel-stack" : ""}`}
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
        {activeControlPanelTab ? (
          renderControlPanel(pane.id)
        ) : activeSourceTab ? (
          <SourcePanel
            candidatePaths={candidateSourcePaths}
            editorAppearance={editorAppearance}
            fileState={fileState}
            sourceDraft={sourceDraft}
            sourcePath={activeSourceTab.path}
            onDraftChange={setSourceDraft}
            onOpenPath={(path) => onPaneSourcePathChange(pane.id, path)}
            onSaveFile={handleSourceFileSave}
          />
        ) : activeFilesystemTab ? (
          <FileSystemPanel
            rootPath={activeFilesystemTab.rootPath}
            onOpenPath={(path) => onOpenSourceTab(pane.id, path, activeSession?.id ?? null)}
            onOpenRootPath={(path) =>
              onOpenFilesystemTab(pane.id, path, activeSession?.id ?? null)
            }
          />
        ) : activeGitStatusTab ? (
          <GitStatusPanel
            workdir={activeGitStatusTab.workdir}
            onOpenPath={(path) => onOpenSourceTab(pane.id, path, activeSession?.id ?? null)}
            onOpenWorkdir={(path) =>
              onOpenGitStatusTab(pane.id, path, activeSession?.id ?? null)
            }
          />
        ) : activeDiffPreviewTab ? (
          <DiffPanel
            appearance={editorAppearance}
            changeType={activeDiffPreviewTab.changeType}
            diff={activeDiffPreviewTab.diff}
            diffMessageId={activeDiffPreviewTab.diffMessageId}
            filePath={activeDiffPreviewTab.filePath}
            language={activeDiffPreviewTab.language ?? null}
            onOpenPath={(path) => onOpenSourceTab(pane.id, path, activeSession?.id ?? null)}
            summary={activeDiffPreviewTab.summary}
          />
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
            onCancelQueuedPrompt={onCancelQueuedPrompt}
            onSessionSettingsChange={onSessionSettingsChange}
            conversationSearchQuery={hasSessionFindQuery ? sessionFindQuery : ""}
            conversationSearchMatchedItemKeys={sessionSearchMatchedItemKeys}
            conversationSearchActiveItemKey={activeSessionSearchMatch?.itemKey ?? null}
            onConversationSearchItemMount={handleConversationSearchItemMount}
            renderCommandCard={(message) => <CommandCard message={message} />}
            renderDiffCard={(message) => (
              <DiffCard
                message={message}
                onOpenPreview={() =>
                  onOpenDiffPreviewTab(pane.id, message, activeSession?.id ?? null)
                }
              />
            )}
            renderMessageCard={(message, preferImmediateHeavyRender, handleDecision) => (
              <MessageCard
                message={message}
                onOpenDiffPreview={(diffMessage) =>
                  onOpenDiffPreviewTab(pane.id, diffMessage, activeSession?.id ?? null)
                }
                preferImmediateHeavyRender={preferImmediateHeavyRender}
                onApprovalDecision={handleDecision}
                searchQuery={
                  activeSessionSearchMatch?.itemKey === `message:${message.id}` ? sessionFindQuery : ""
                }
                searchHighlightTone={
                  activeSessionSearchMatch?.itemKey === `message:${message.id}` ? "active" : "match"
                }
              />
            )}
            renderPromptSettings={(panelPaneId, session, panelIsUpdating, handleSettingsChange) => {
              if (session.agent === "Codex") {
                return (
                  <CodexPromptSettingsCard
                    paneId={panelPaneId}
                    session={session}
                    isUpdating={panelIsUpdating}
                    onSessionSettingsChange={handleSettingsChange}
                  />
                );
              }

              if (session.agent === "Claude") {
                return (
                  <ClaudePromptSettingsCard
                    paneId={panelPaneId}
                    session={session}
                    isUpdating={panelIsUpdating}
                    onSessionSettingsChange={handleSettingsChange}
                  />
                );
              }

              return null;
            }}
          />
        )}
      </section>
      {activeControlPanelTab || activeSourceTab || activeFilesystemTab || activeGitStatusTab || activeDiffPreviewTab ? null : (
        <AgentSessionPanelFooter
          paneId={pane.id}
          viewMode={pane.viewMode}
          activeSession={activeSession}
          committedDraft={draft}
          draftAttachments={draftAttachments}
          formatByteSize={formatByteSize}
          isSending={isSending}
          isStopping={isStopping}
          isSessionBusy={isSessionBusy}
          showNewResponseIndicator={showNewResponseIndicator}
          footerModeLabel={labelForPaneViewMode(pane.viewMode)}
          onScrollToLatest={() => scrollToLatestMessage("smooth")}
          onDraftCommit={onDraftCommit}
          onDraftAttachmentRemove={onDraftAttachmentRemove}
          onSend={onSend}
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
    return paneLookup.get(node.paneId)?.tabs.some((tab) => tab.kind === "controlPanel") ?? false;
  }

  return (
    workspaceNodeContainsControlPanel(node.first, paneLookup) ||
    workspaceNodeContainsControlPanel(node.second, paneLookup)
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
  const currentMatch = hasMatches && activeIndex >= 0 ? matches[activeIndex] ?? null : null;
  const countLabel = !hasQuery
    ? "Type to search"
    : hasMatches
      ? `${activeIndex + 1} of ${matches.length}`
      : "No matches";

  return (
    <div className="session-find-bar" role="search" aria-label="Find in session">
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
      <button className="session-find-button session-find-close" type="button" onClick={onClose}>
        Close
      </button>
    </div>
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

const MessageCard = memo(function MessageCard({
  message,
  onOpenDiffPreview,
  preferImmediateHeavyRender = false,
  onApprovalDecision,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: Message;
  onOpenDiffPreview?: (message: DiffMessage) => void;
  preferImmediateHeavyRender?: boolean;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  switch (message.type) {
    case "text": {
      const connectionRetryNotice =
        message.author === "assistant" ? parseConnectionRetryNotice(message.text) : null;

      if (connectionRetryNotice) {
        return (
          <ConnectionRetryCard
            message={message}
            notice={connectionRetryNotice}
            searchQuery={searchQuery}
            searchHighlightTone={searchHighlightTone}
          />
        );
      }

      return (
        <article className={`message-card bubble bubble-${message.author}`}>
          <MessageMeta author={message.author} timestamp={message.timestamp} />
          {message.attachments && message.attachments.length > 0 ? (
            <MessageAttachmentList
              attachments={message.attachments}
              searchQuery={searchQuery}
              searchHighlightTone={searchHighlightTone}
            />
          ) : null}
          {message.author === "assistant" ? (
            preferImmediateHeavyRender ? (
              <MarkdownContent
                markdown={message.text}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            ) : (
              <DeferredMarkdownContent
                markdown={message.text}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            )
          ) : message.text ? (
            <p className="plain-text-copy">
              {renderHighlightedText(message.text, searchQuery, searchHighlightTone)}
            </p>
          ) : (
            <p className="support-copy">{imageAttachmentSummaryLabel(message.attachments?.length ?? 0)}</p>
          )}
        </article>
      );
    }
    case "thinking":
      return (
        <ThinkingCard
          message={message}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "command":
      return (
        <CommandCard
          message={message}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "diff":
      return (
        <DiffCard
          message={message}
          onOpenPreview={() => onOpenDiffPreview?.(message)}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "markdown":
      return (
        <MarkdownCard
          message={message}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    case "approval":
      return (
        <ApprovalCard
          message={message}
          onApprovalDecision={onApprovalDecision}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      );
    default:
      return null;
  }
}, (previous, next) =>
  previous.message === next.message &&
  previous.onOpenDiffPreview === next.onOpenDiffPreview &&
  previous.preferImmediateHeavyRender === next.preferImmediateHeavyRender &&
  previous.searchQuery === next.searchQuery &&
  previous.searchHighlightTone === next.searchHighlightTone
);

function ConnectionRetryCard({
  message,
  notice,
  searchQuery,
  searchHighlightTone,
}: {
  message: TextMessage;
  notice: ConnectionRetryNotice;
  searchQuery: string;
  searchHighlightTone: SearchHighlightTone;
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
          <p className="connection-notice-detail">
            {renderHighlightedText(notice.detail, searchQuery, searchHighlightTone)}
          </p>
        </div>
      </div>
    </article>
  );
}

function MessageAttachmentList({
  attachments,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  attachments: ImageAttachment[];
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  return (
    <div className="message-attachment-list">
      {attachments.map((attachment, index) => (
        <div key={`${attachment.fileName}-${attachment.byteSize}-${index}`} className="message-attachment-chip">
          <strong className="message-attachment-name">
            {renderHighlightedText(attachment.fileName, searchQuery, searchHighlightTone)}
          </strong>
          <span className="message-attachment-meta">
            {formatByteSize(attachment.byteSize)} ·{" "}
            {renderHighlightedText(attachment.mediaType, searchQuery, searchHighlightTone)}
          </span>
        </div>
      ))}
    </div>
  );
}

function MessageMeta({
  author,
  timestamp,
  trailing,
}: {
  author: string;
  timestamp: string;
  trailing?: ReactNode;
}) {
  return (
    <div className="message-meta">
      <span>{author === "you" ? "You" : "Agent"}</span>
      <span className="message-meta-end">
        {trailing}
        <span>{timestamp}</span>
      </span>
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
  searchQuery,
  searchHighlightTone = "match",
}: {
  className: string;
  code: string;
  commandHint?: string | null;
  language?: string | null;
  pathHint?: string | null;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const metrics = useMemo(() => measureTextBlock(code), [code]);
  const showSearchHighlight = containsSearchMatch(code, searchQuery ?? "");
  const shouldDefer =
    !showSearchHighlight &&
    (metrics.lineCount >= HEAVY_CODE_LINE_THRESHOLD || code.length >= HEAVY_CODE_CHARACTER_THRESHOLD);

  if (!shouldDefer) {
    return (
      <HighlightedCodeBlock
        className={className}
        code={code}
        commandHint={commandHint}
        language={language}
        pathHint={pathHint}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
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
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
    </DeferredHeavyContent>
  );
}

function DeferredMarkdownContent({
  markdown,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  markdown: string;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const metrics = useMemo(() => measureTextBlock(markdown), [markdown]);
  const showSearchHighlight = containsSearchMatch(markdown, searchQuery);
  const shouldDefer =
    !showSearchHighlight &&
    (metrics.lineCount >= HEAVY_MARKDOWN_LINE_THRESHOLD ||
      markdown.length >= HEAVY_MARKDOWN_CHARACTER_THRESHOLD);

  if (!shouldDefer) {
    return (
      <MarkdownContent
        markdown={markdown}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
    );
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
      <MarkdownContent
        markdown={markdown}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
    </DeferredHeavyContent>
  );
}

function HighlightedCodeBlock({
  className,
  code,
  commandHint,
  language,
  pathHint,
  showCopyButton = false,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  className: string;
  code: string;
  commandHint?: string | null;
  language?: string | null;
  pathHint?: string | null;
  showCopyButton?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [copied, setCopied] = useState(false);
  const showSearchHighlight = containsSearchMatch(code, searchQuery);
  const highlighted = useMemo(
    () =>
      highlightCode(code, {
        commandHint,
        language,
        pathHint,
      }),
    [code, commandHint, language, pathHint],
  );

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
      await copyTextToClipboard(code);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <pre className={`${className} syntax-block${showCopyButton ? " copyable" : ""}`}>
      {showCopyButton ? (
        <button
          className={`command-icon-button syntax-copy-button${copied ? " copied" : ""}`}
          type="button"
          onMouseDown={(event) => {
            event.preventDefault();
            event.stopPropagation();
          }}
          onClick={() => void handleCopy()}
          aria-label={copied ? "Code copied" : "Copy code"}
          title={copied ? "Copied" : "Copy code"}
        >
          {copied ? <CheckIcon /> : <CopyIcon />}
        </button>
      ) : null}
      <code className={`hljs${highlighted.language ? ` language-${highlighted.language}` : ""}`}>
        {showSearchHighlight
          ? renderHighlightedText(code, searchQuery, searchHighlightTone)
          : (
            <span dangerouslySetInnerHTML={{ __html: highlighted.html }} />
          )}
      </code>
    </pre>
  );
}

function ThinkingCard({
  message,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: ThinkingMessage;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  return (
    <article className="message-card reasoning-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Thinking</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <ul className="plain-list">
        {message.lines.map((line) => (
          <li key={line}>{renderHighlightedText(line, searchQuery, searchHighlightTone)}</li>
        ))}
      </ul>
    </article>
  );
}

function CommandCard({
  message,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: CommandMessage;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [inputExpanded, setInputExpanded] = useState(false);
  const [outputExpanded, setOutputExpanded] = useState(false);
  const [copiedSection, setCopiedSection] = useState<"command" | "output" | null>(null);
  const hasOutput = message.output.trim().length > 0;
  const displayOutput = hasOutput
    ? message.output
    : message.status === "running"
      ? "Awaiting output…"
      : "No output";
  const canExpandCommand =
    message.command.split("\n").length > 10 || message.command.length > 480;
  const canExpandOutput =
    hasOutput && (message.output.split("\n").length > 10 || message.output.length > 480);
  const statusTone = mapCommandStatus(message.status);
  const isSearchExpanded = searchQuery.trim().length > 0;
  const isInputExpanded = inputExpanded || isSearchExpanded;
  const isOutputExpanded = outputExpanded || isSearchExpanded;

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
      <MessageMeta
        author={message.author}
        timestamp={message.timestamp}
        trailing={
          <span className={`chip chip-status chip-status-${statusTone} command-status-chip`}>
            {message.status}
          </span>
        }
      />
      <div className="card-label command-card-label">Command</div>

      <div className="command-panel">
        <div className="command-row">
          <div className="command-row-label">IN</div>
          <div className="command-row-body">
            <div className={`command-input-shell ${isInputExpanded ? "expanded" : "collapsed"}`}>
              <DeferredHighlightedCodeBlock
                className="command-text command-text-input"
                code={message.command}
                language={message.commandLanguage ?? "bash"}
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            </div>
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
            {canExpandCommand ? (
              <button
                className="command-icon-button"
                type="button"
                onClick={() => setInputExpanded((open) => !open)}
                aria-label={isInputExpanded ? "Collapse command" : "Expand command"}
                aria-pressed={isInputExpanded}
                title={isInputExpanded ? "Collapse command" : "Expand command"}
              >
                {isInputExpanded ? <CollapseIcon /> : <ExpandIcon />}
              </button>
            ) : null}
          </div>
        </div>

        <div className="command-row command-row-output">
          <div className="command-row-label">OUT</div>
          <div className="command-row-body">
            <div
              className={`command-output-shell ${isOutputExpanded ? "expanded" : "collapsed"} ${hasOutput ? "has-output" : "empty"}`}
            >
              {hasOutput ? (
                <DeferredHighlightedCodeBlock
                  className="command-text command-text-output"
                  code={displayOutput}
                  language={message.outputLanguage ?? null}
                  commandHint={message.output ? message.command : null}
                  searchQuery={searchQuery}
                  searchHighlightTone={searchHighlightTone}
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
                onClick={() => setOutputExpanded((open) => !open)}
                aria-label={isOutputExpanded ? "Collapse output" : "Expand output"}
                aria-pressed={isOutputExpanded}
                title={isOutputExpanded ? "Collapse output" : "Expand output"}
              >
                {isOutputExpanded ? <CollapseIcon /> : <ExpandIcon />}
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

function DiffCard({
  message,
  onOpenPreview,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: DiffMessage;
  onOpenPreview: () => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const canExpandDiff = message.diff.split("\n").length > 14 || message.diff.length > 900;
  const isExpanded = !canExpandDiff || expanded || searchQuery.trim().length > 0;

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
            <div className="diff-file-path">
              {renderHighlightedText(message.filePath, searchQuery, searchHighlightTone)}
            </div>
            <p className="diff-file-summary">
              {renderHighlightedText(message.summary, searchQuery, searchHighlightTone)}
            </p>
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
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            </div>
          </div>
          <div className="command-row-actions">
            <button
              className="command-icon-button"
              type="button"
              onClick={onOpenPreview}
              aria-label="Open diff preview"
              title="Open diff preview"
            >
              <PreviewIcon />
            </button>
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

function PreviewIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M8 3c3.38 0 6.18 2.35 7 5-.82 2.65-3.62 5-7 5S1.82 10.65 1 8c.82-2.65 3.62-5 7-5Zm0 1C5.2 4 2.82 5.82 2.05 8 2.82 10.18 5.2 12 8 12s5.18-1.82 5.95-4C13.18 5.82 10.8 4 8 4Zm0 1.5A2.5 2.5 0 1 1 5.5 8 2.5 2.5 0 0 1 8 5.5Zm0 1A1.5 1.5 0 1 0 9.5 8 1.5 1.5 0 0 0 8 6.5Z"
        fill="currentColor"
      />
    </svg>
  );
}

function MarkdownCard({
  message,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: MarkdownMessage;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  return (
    <article className="message-card markdown-card">
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Markdown</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <DeferredMarkdownContent
        markdown={message.markdown}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
    </article>
  );
}

function ApprovalCard({
  message,
  onApprovalDecision,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  message: ApprovalMessage;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const decided = message.decision !== "pending";
  const chosen = (d: ApprovalDecision) => (message.decision === d ? " chosen" : "");
  const resolvedDecision = message.decision === "pending" ? null : message.decision;

  return (
    <article className={`message-card approval-card${decided ? " decided" : ""}`}>
      <MessageMeta author={message.author} timestamp={message.timestamp} />
      <div className="card-label">Approval</div>
      <h3>{renderHighlightedText(message.title, searchQuery, searchHighlightTone)}</h3>
      <DeferredHighlightedCodeBlock
        className="approval-command"
        code={message.command}
        language={message.commandLanguage ?? "bash"}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
      />
      <p className="support-copy">
        {renderHighlightedText(message.detail, searchQuery, searchHighlightTone)}
      </p>
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

function MarkdownContent({
  markdown,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  markdown: string;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  const highlightChildren = (children: ReactNode) =>
    highlightReactNodeText(children, searchQuery, searchHighlightTone);

  return (
    <div className="markdown-copy">
      <ReactMarkdown
        components={{
          a: ({ href, children, ...props }) => (
            <a
              {...props}
              href={href}
              target={href?.startsWith("http") ? "_blank" : undefined}
              rel={href?.startsWith("http") ? "noreferrer" : undefined}
            >
              {highlightChildren(children)}
            </a>
          ),
          code: ({ children, className, inline, ...props }) => {
            const language = className?.match(/language-([\w-]+)/)?.[1] ?? null;
            const code = String(children).replace(/\n$/, "");

            if (inline) {
              return (
                <code className={className} {...props}>
                  {highlightChildren(children)}
                </code>
              );
            }

            return (
              <HighlightedCodeBlock
                className="code-block"
                code={code}
                language={language}
                showCopyButton
                searchQuery={searchQuery}
                searchHighlightTone={searchHighlightTone}
              />
            );
          },
          p: ({ children, ...props }) => <p {...props}>{highlightChildren(children)}</p>,
          li: ({ children, ...props }) => <li {...props}>{highlightChildren(children)}</li>,
          blockquote: ({ children, ...props }) => (
            <blockquote {...props}>{highlightChildren(children)}</blockquote>
          ),
          h1: ({ children, ...props }) => <h1 {...props}>{highlightChildren(children)}</h1>,
          h2: ({ children, ...props }) => <h2 {...props}>{highlightChildren(children)}</h2>,
          h3: ({ children, ...props }) => <h3 {...props}>{highlightChildren(children)}</h3>,
          h4: ({ children, ...props }) => <h4 {...props}>{highlightChildren(children)}</h4>,
          h5: ({ children, ...props }) => <h5 {...props}>{highlightChildren(children)}</h5>,
          h6: ({ children, ...props }) => <h6 {...props}>{highlightChildren(children)}</h6>,
          strong: ({ children, ...props }) => (
            <strong {...props}>{highlightChildren(children)}</strong>
          ),
          em: ({ children, ...props }) => <em {...props}>{highlightChildren(children)}</em>,
          del: ({ children, ...props }) => <del {...props}>{highlightChildren(children)}</del>,
          td: ({ children, ...props }) => <td {...props}>{highlightChildren(children)}</td>,
          th: ({ children, ...props }) => <th {...props}>{highlightChildren(children)}</th>,
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
    case "controlPanel":
      return "Control panel";
    case "source":
      return "Source";
    case "filesystem":
      return "Files";
    case "gitStatus":
      return "Git status";
    case "diffPreview":
      return "Diff preview";
  }
}

function isHexColorDark(value: string) {
  const hex = value.trim().replace(/^#/, "");
  if (hex.length !== 6) {
    return false;
  }

  const red = Number.parseInt(hex.slice(0, 2), 16);
  const green = Number.parseInt(hex.slice(2, 4), 16);
  const blue = Number.parseInt(hex.slice(4, 6), 16);
  const luminance = (red * 299 + green * 587 + blue * 114) / 1000;
  return luminance < 148;
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

function primaryModifierLabel() {
  if (typeof navigator === "undefined") {
    return "Ctrl";
  }

  return navigator.platform.toLowerCase().includes("mac") ? "Cmd" : "Ctrl";
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
