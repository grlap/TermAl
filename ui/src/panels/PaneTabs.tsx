import {
  Fragment,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type DragEvent as ReactDragEvent,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { createPortal } from "react-dom";
import {
  fetchGitStatus,
  pushGitChanges,
  syncGitChanges,
  type GitStatusResponse,
} from "../api";
import { AgentIcon } from "../agent-icon";
import { copyTextToClipboard } from "../clipboard";
import { FileTabIcon } from "../file-tab-icon";
import {
  looksLikeAbsoluteDisplayPath,
  normalizeDisplayPath,
  relativizePathToWorkspace,
} from "../path-display";
import {
  createBuiltinLocalRemote,
  isLocalRemoteId,
  remoteConnectionLabel,
  remoteDisplayName,
  resolveProjectRemoteId,
} from "../remotes";
import { matchingSessionModelOption } from "../session-model-options";
import {
  attachWorkspaceTabDragData,
  createWorkspaceTabDrag,
  type WorkspaceTabDrag,
} from "../tab-drag";
import { measurePaneTabStatusTooltipPosition } from "../pane-tab-status-tooltip";
import type { CodexRateLimitWindow, CodexState, Project, RemoteConfig, Session } from "../types";
import type { TabDropPlacement, WorkspaceGitStatusTab, WorkspaceTab } from "../workspace";

type ActiveSessionTooltipState = {
  id: string;
  sessionId: string;
};

type FileTabContextMenuState = {
  clientX: number;
  clientY: number;
  paneId: string;
  path: string;
  relativePath: string | null;
  tabId: string;
};

type GitTabContextMenuAction = "push" | "sync";

type GitTabContextMenuState = {
  clientX: number;
  clientY: number;
  isLoadingStatus: boolean;
  pendingAction: GitTabContextMenuAction | null;
  projectId: string | null;
  sessionId: string | null;
  status: GitStatusResponse | null;
  statusError: string | null;
  statusMessage: string | null;
  tabId: string;
  workdir: string;
};

export function PaneTabs({
  paneId,
  windowId,
  tabs,
  activeTabId,
  codexState,
  projectLookup,
  remoteLookup,
  sessionLookup,
  draggedTab,
  onSelectTab,
  onCloseTab,
  onTabDragStart,
  onTabDragEnd,
  onTabDrop,
  onRenameSessionRequest,
}: {
  paneId: string;
  windowId: string;
  tabs: WorkspaceTab[];
  activeTabId: string | null;
  codexState: CodexState;
  projectLookup: Map<string, Project>;
  remoteLookup: Map<string, RemoteConfig>;
  sessionLookup: Map<string, Session>;
  draggedTab: WorkspaceTabDrag | null;
  onSelectTab: (paneId: string, tabId: string) => void;
  onCloseTab: (paneId: string, tabId: string) => void;
  onTabDragStart: (drag: WorkspaceTabDrag) => void;
  onTabDragEnd: () => void;
  onTabDrop: (targetPaneId: string, placement: TabDropPlacement, tabIndex?: number) => void;
  onRenameSessionRequest: (
    sessionId: string,
    clientX: number,
    clientY: number,
    trigger?: HTMLElement | null,
  ) => void;
}) {
  const paneTabsRef = useRef<HTMLDivElement | null>(null);
  const activeStatusTooltipAnchorRef = useRef<HTMLElement | null>(null);
  const fileTabContextMenuRef = useRef<HTMLDivElement | null>(null);
  const gitTabContextMenuRef = useRef<HTMLDivElement | null>(null);
  const gitTabContextMenuRequestIdRef = useRef(0);
  const paneHasControlPanel = tabs.some((tab) => tab.kind === "controlPanel");
  const codexNotices = codexState.notices ?? [];
  const hasCodexNotices = codexNotices.length > 0;
  const canDropInTabRail = draggedTab !== null &&
    draggedTab.tab.kind !== "controlPanel" &&
    !paneHasControlPanel;
  const [tabRailState, setTabRailState] = useState({
    hasOverflow: false,
    canScrollPrev: false,
    canScrollNext: false,
  });
  const [activeTabInsertIndex, setActiveTabInsertIndex] = useState<number | null>(null);
  const [activeStatusTooltip, setActiveStatusTooltip] = useState<ActiveSessionTooltipState | null>(null);
  const [activeStatusTooltipStyle, setActiveStatusTooltipStyle] = useState<CSSProperties | null>(null);
  const [fileTabContextMenu, setFileTabContextMenu] = useState<FileTabContextMenuState | null>(null);
  const [fileTabContextMenuStyle, setFileTabContextMenuStyle] = useState<CSSProperties | null>(null);
  const [gitTabContextMenu, setGitTabContextMenu] = useState<GitTabContextMenuState | null>(null);
  const [gitTabContextMenuStyle, setGitTabContextMenuStyle] = useState<CSSProperties | null>(null);

  function updateActiveStatusTooltipPosition(anchor = activeStatusTooltipAnchorRef.current) {
    if (!anchor || typeof window === "undefined") {
      setActiveStatusTooltipStyle(null);
      return;
    }

    const position = measurePaneTabStatusTooltipPosition(anchor.getBoundingClientRect(), window.innerWidth);
    const nextStyle = {
      left: `${position.left}px`,
      top: `${position.top}px`,
      width: `${position.width}px`,
      ["--pane-tab-status-arrow-left"]: `${position.arrowLeft}px`,
    } as CSSProperties;
    setActiveStatusTooltipStyle(nextStyle);
  }

  function openStatusTooltip(id: string, sessionId: string, anchor: HTMLElement) {
    activeStatusTooltipAnchorRef.current = anchor;
    setActiveStatusTooltip((current) =>
      current?.id === id && current.sessionId === sessionId ? current : { id, sessionId },
    );
    updateActiveStatusTooltipPosition(anchor);
  }

  function closeStatusTooltip() {
    activeStatusTooltipAnchorRef.current = null;
    setActiveStatusTooltip(null);
    setActiveStatusTooltipStyle(null);
  }

  function closeFileTabContextMenu() {
    setFileTabContextMenu(null);
    setFileTabContextMenuStyle(null);
  }

  function closeGitTabContextMenu() {
    gitTabContextMenuRequestIdRef.current += 1;
    setGitTabContextMenu(null);
    setGitTabContextMenuStyle(null);
  }

  function patchGitTabContextMenu(
    updater: (current: GitTabContextMenuState) => GitTabContextMenuState,
  ) {
    setGitTabContextMenu((current) => (current ? updater(current) : current));
  }

  async function handleGitTabRepoAction(action: GitTabContextMenuAction) {
    if (!gitTabContextMenu || gitTabContextMenu.pendingAction || gitTabContextMenu.isLoadingStatus) {
      return;
    }

    const requestId = gitTabContextMenuRequestIdRef.current;
    const { projectId, sessionId, workdir } = gitTabContextMenu;
    patchGitTabContextMenu((current) => ({
      ...current,
      pendingAction: action,
      statusError: null,
      statusMessage: null,
    }));

    try {
      const response = action === "push"
        ? await pushGitChanges({ projectId, sessionId, workdir })
        : await syncGitChanges({ projectId, sessionId, workdir });

      if (gitTabContextMenuRequestIdRef.current !== requestId) {
        return;
      }

      patchGitTabContextMenu((current) => ({
        ...current,
        pendingAction: null,
        status: response.status,
        statusError: null,
        statusMessage: response.summary,
      }));
    } catch (error) {
      if (gitTabContextMenuRequestIdRef.current !== requestId) {
        return;
      }

      patchGitTabContextMenu((current) => ({
        ...current,
        pendingAction: null,
        statusError: formatGitTabContextMenuError(error),
      }));
    }
  }

  function updateFileTabContextMenuPosition(
    menu = fileTabContextMenu,
    node = fileTabContextMenuRef.current,
  ) {
    if (!menu || !node || typeof window === "undefined") {
      setFileTabContextMenuStyle(null);
      return;
    }

    const menuRect = node.getBoundingClientRect();
    const viewportPadding = 12;
    const left = Math.max(
      viewportPadding,
      Math.min(menu.clientX, window.innerWidth - menuRect.width - viewportPadding),
    );
    const top = Math.max(
      viewportPadding,
      Math.min(menu.clientY, window.innerHeight - menuRect.height - viewportPadding),
    );

    setFileTabContextMenuStyle({
      left: `${left}px`,
      top: `${top}px`,
    });
  }

  function updateGitTabContextMenuPosition(
    menu = gitTabContextMenu,
    node = gitTabContextMenuRef.current,
  ) {
    if (!menu || !node || typeof window === "undefined") {
      setGitTabContextMenuStyle(null);
      return;
    }

    const menuRect = node.getBoundingClientRect();
    const viewportPadding = 12;
    const left = Math.max(
      viewportPadding,
      Math.min(menu.clientX, window.innerWidth - menuRect.width - viewportPadding),
    );
    const top = Math.max(
      viewportPadding,
      Math.min(menu.clientY, window.innerHeight - menuRect.height - viewportPadding),
    );

    setGitTabContextMenuStyle({
      left: `${left}px`,
      top: `${top}px`,
    });
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

  function handleTabRailWheel(event: ReactWheelEvent<HTMLDivElement>) {
    const node = paneTabsRef.current;
    if (!node || event.ctrlKey) {
      return;
    }

    const maxScrollLeft = Math.max(node.scrollWidth - node.clientWidth, 0);
    if (maxScrollLeft <= 0) {
      return;
    }

    const dominantDelta =
      Math.abs(event.deltaX) > Math.abs(event.deltaY) ? event.deltaX : event.deltaY;
    if (Math.abs(dominantDelta) < 0.5) {
      return;
    }

    const nextScrollLeft = Math.min(Math.max(node.scrollLeft + dominantDelta, 0), maxScrollLeft);
    if (Math.abs(nextScrollLeft - node.scrollLeft) < 0.5) {
      return;
    }

    event.preventDefault();
    node.scrollLeft = nextScrollLeft;
  }

  function resolveTabInsertIndex(clientX: number) {
    const node = paneTabsRef.current;
    if (!node) {
      return tabs.length;
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
    if (!draggedTab || !canDropInTabRail) {
      setActiveTabInsertIndex(null);
      return;
    }

    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    maybeAutoScrollTabRail(event.clientX);

    const nextTabInsertIndex = resolveTabInsertIndex(event.clientX);
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
    if (!draggedTab || !canDropInTabRail) {
      return;
    }

    event.preventDefault();
    setActiveTabInsertIndex(null);
    onTabDrop(paneId, "tabs", resolveTabInsertIndex(event.clientX));
  }

  useLayoutEffect(() => {
    const frameId = window.requestAnimationFrame(() => {
      const node = paneTabsRef.current;
      if (!node) {
        return;
      }

      const activeTab = node.querySelector<HTMLElement>('.pane-tab-shell[aria-selected="true"]');
      if (typeof activeTab?.scrollIntoView === "function") {
        activeTab.scrollIntoView({
          block: "nearest",
          inline: "nearest",
        });
      }
      updateTabRailState();
    });

    return () => {
      window.cancelAnimationFrame(frameId);
    };
  }, [activeTabId, paneId, tabs.length]);

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
  }, [paneId, tabs.length]);

  useEffect(() => {
    if (!draggedTab) {
      setActiveTabInsertIndex(null);
    }
  }, [draggedTab]);

  useEffect(() => {
    if (!fileTabContextMenu) {
      return;
    }

    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Node && fileTabContextMenuRef.current?.contains(target)) {
        return;
      }

      closeFileTabContextMenu();
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        closeFileTabContextMenu();
      }
    };
    const handleViewportChange = () => {
      closeFileTabContextMenu();
    };

    window.addEventListener("pointerdown", handlePointerDown, true);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("resize", handleViewportChange);
    window.addEventListener("scroll", handleViewportChange, true);

    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("resize", handleViewportChange);
      window.removeEventListener("scroll", handleViewportChange, true);
    };
  }, [fileTabContextMenu]);


  useEffect(() => {
    if (!gitTabContextMenu) {
      return;
    }

    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Node && gitTabContextMenuRef.current?.contains(target)) {
        return;
      }

      closeGitTabContextMenu();
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        closeGitTabContextMenu();
      }
    };
    const handleViewportChange = () => {
      closeGitTabContextMenu();
    };

    window.addEventListener("pointerdown", handlePointerDown, true);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("resize", handleViewportChange);
    window.addEventListener("scroll", handleViewportChange, true);

    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("resize", handleViewportChange);
      window.removeEventListener("scroll", handleViewportChange, true);
    };
  }, [gitTabContextMenu]);

  useEffect(() => {
    if (!fileTabContextMenu) {
      return;
    }

    if (!tabs.some((tab) => tab.id === fileTabContextMenu.tabId)) {
      closeFileTabContextMenu();
    }
  }, [fileTabContextMenu, tabs]);


  useEffect(() => {
    if (!gitTabContextMenu) {
      return;
    }

    if (!tabs.some((tab) => tab.id === gitTabContextMenu.tabId)) {
      closeGitTabContextMenu();
    }
  }, [gitTabContextMenu, tabs]);

  useEffect(() => {
    if (!gitTabContextMenu) {
      return;
    }

    const requestId = gitTabContextMenuRequestIdRef.current;
    let cancelled = false;

    void fetchGitStatus(gitTabContextMenu.workdir, gitTabContextMenu.sessionId, {
      projectId: gitTabContextMenu.projectId,
    })
      .then((status) => {
        if (cancelled || gitTabContextMenuRequestIdRef.current !== requestId) {
          return;
        }

        patchGitTabContextMenu((current) => ({
          ...current,
          isLoadingStatus: false,
          status,
          statusError: null,
        }));
      })
      .catch((error) => {
        if (cancelled || gitTabContextMenuRequestIdRef.current !== requestId) {
          return;
        }

        patchGitTabContextMenu((current) => ({
          ...current,
          isLoadingStatus: false,
          statusError: formatGitTabContextMenuError(error),
        }));
      });

    return () => {
      cancelled = true;
    };
  }, [
    gitTabContextMenu?.projectId,
    gitTabContextMenu?.sessionId,
    gitTabContextMenu?.tabId,
    gitTabContextMenu?.workdir,
  ]);

  useLayoutEffect(() => {
    if (!activeStatusTooltip) {
      return;
    }

    const updateTooltipPosition = () => {
      const anchor = activeStatusTooltipAnchorRef.current;
      if (!anchor || !anchor.isConnected) {
        closeStatusTooltip();
        return;
      }

      updateActiveStatusTooltipPosition(anchor);
    };

    updateTooltipPosition();

    const node = paneTabsRef.current;
    window.addEventListener("resize", updateTooltipPosition);
    window.addEventListener("scroll", updateTooltipPosition, true);
    node?.addEventListener("scroll", updateTooltipPosition, { passive: true });

    return () => {
      window.removeEventListener("resize", updateTooltipPosition);
      window.removeEventListener("scroll", updateTooltipPosition, true);
      node?.removeEventListener("scroll", updateTooltipPosition);
    };
  }, [activeStatusTooltip, activeTabId, tabs.length]);

  useLayoutEffect(() => {
    if (!fileTabContextMenu) {
      return;
    }

    updateFileTabContextMenuPosition();
  }, [fileTabContextMenu]);


  useLayoutEffect(() => {
    if (!gitTabContextMenu) {
      return;
    }

    updateGitTabContextMenuPosition();
  }, [gitTabContextMenu]);

  useEffect(() => {
    if (!activeStatusTooltip || sessionLookup.has(activeStatusTooltip.sessionId)) {
      return;
    }

    closeStatusTooltip();
  }, [activeStatusTooltip, sessionLookup]);

  const activeStatusTooltipSession = activeStatusTooltip
    ? (sessionLookup.get(activeStatusTooltip.sessionId) ?? null)
    : null;

  return (
    <div className="pane-tabs-shell">
      {tabs.length > 1 ? (
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
        className={`pane-tabs ${activeTabInsertIndex === 0 && tabs.length === 0 ? "drop-empty" : ""}`}
        role="tablist"
        aria-label="Tile tabs"
        onDragOver={handleTabRailDragOver}
        onDragLeave={handleTabRailDragLeave}
        onDrop={handleTabRailDrop}
        onWheel={handleTabRailWheel}
      >
        {tabs.length > 0 ? (
          tabs.map((tab, index) => {
            const session = tab.kind === "session" ? (sessionLookup.get(tab.sessionId) ?? null) : null;
            const tabActive = tab.id === activeTabId;
            const showStatusTooltip = Boolean(session && hasSessionTabStatusTooltip(session));
            const showDropBefore = activeTabInsertIndex === index;
            const showDropAfter = activeTabInsertIndex === tabs.length && index === tabs.length - 1;
            const statusTooltipId = showStatusTooltip ? `session-status-${paneId}-${tab.id}` : undefined;
            const tabLabel = formatTabLabel(tab, session);

            return (
              <div
                key={tab.id}
                className={`pane-tab-shell ${tabActive ? "active" : ""} ${showStatusTooltip ? "has-status-tooltip" : ""} ${showDropBefore ? "drop-before" : ""} ${showDropAfter ? "drop-after" : ""}`}
                role="tab"
                aria-selected={tabActive}
                aria-describedby={activeStatusTooltip?.id === statusTooltipId ? statusTooltipId : undefined}
                tabIndex={0}
                onMouseEnter={(event) => {
                  if (showStatusTooltip && session && statusTooltipId) {
                    openStatusTooltip(statusTooltipId, session.id, event.currentTarget);
                  }
                }}
                onMouseLeave={() => {
                  closeStatusTooltip();
                }}
                onFocus={(event) => {
                  if (showStatusTooltip && session && statusTooltipId) {
                    openStatusTooltip(statusTooltipId, session.id, event.currentTarget);
                  }
                }}
                onBlur={(event) => {
                  const nextTarget = event.relatedTarget;
                  if (nextTarget instanceof Node && event.currentTarget.contains(nextTarget)) {
                    return;
                  }

                  closeStatusTooltip();
                }}
                onClick={() => onSelectTab(paneId, tab.id)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    onSelectTab(paneId, tab.id);
                  }
                }}
                onContextMenu={(event) => {
                  closeFileTabContextMenu();
                  closeGitTabContextMenu();

                  const gitTabContext = buildGitTabContextMenu(tab, sessionLookup, projectLookup);
                  if (gitTabContext) {
                    event.preventDefault();
                    gitTabContextMenuRequestIdRef.current += 1;
                    setGitTabContextMenu({
                      clientX: event.clientX,
                      clientY: event.clientY,
                      isLoadingStatus: true,
                      pendingAction: null,
                      projectId: gitTabContext.projectId,
                      sessionId: gitTabContext.sessionId,
                      status: null,
                      statusError: null,
                      statusMessage: null,
                      tabId: tab.id,
                      workdir: gitTabContext.workdir,
                    });
                    setGitTabContextMenuStyle({
                      left: `${event.clientX}px`,
                      top: `${event.clientY}px`,
                    });
                    return;
                  }

                  const fileTabContext = buildFileTabContextMenu(tab, sessionLookup, projectLookup);
                  if (fileTabContext) {
                    event.preventDefault();
                    setFileTabContextMenu({
                      clientX: event.clientX,
                      clientY: event.clientY,
                      paneId,
                      path: fileTabContext.path,
                      relativePath: fileTabContext.relativePath,
                      tabId: tab.id,
                    });
                    setFileTabContextMenuStyle({
                      left: `${event.clientX}px`,
                      top: `${event.clientY}px`,
                    });
                    return;
                  }

                  if (!session) {
                    return;
                  }

                  event.preventDefault();
                  onRenameSessionRequest(
                    session.id,
                    event.clientX,
                    event.clientY,
                    event.currentTarget,
                  );
                }}
              >
                <button
                  className="pane-tab-grip"
                  type="button"
                  aria-label={`Drag ${tabLabel}`}
                  title={`Drag ${tabLabel} to move or split`}
                  draggable
                  onMouseDown={(event) => {
                    event.stopPropagation();
                  }}
                  onDragStart={(event) => {
                    const drag = createWorkspaceTabDrag(windowId, paneId, tab);
                    event.dataTransfer.effectAllowed = "move";
                    attachWorkspaceTabDragData(event.dataTransfer, drag);
                    const tabShell = event.currentTarget.closest(".pane-tab-shell");
                    if (tabShell instanceof HTMLElement) {
                      const rect = tabShell.getBoundingClientRect();
                      event.dataTransfer.setDragImage(
                        tabShell,
                        Math.max(12, event.clientX - rect.left),
                        Math.max(12, event.clientY - rect.top),
                      );
                    }
                    closeFileTabContextMenu();
                    closeGitTabContextMenu();
                    onTabDragStart(drag);
                  }}
                  onDragEnd={onTabDragEnd}
                />
                <span className="pane-tab">
                  <span className="pane-tab-copy">
                    {session ? (
                      <span
                        className="status-agent-badge pane-tab-agent-badge"
                        data-status={session.status}
                      >
                        <AgentIcon agent={session.agent} className="pane-tab-agent-icon" />
                      </span>
                    ) : tab.kind === "source" ? (
                      <FileTabIcon path={tab.path} />
                    ) : tab.kind === "diffPreview" ? (
                      <FileTabIcon language={tab.language ?? null} path={tab.filePath} />
                    ) : null}
                    <span className="pane-tab-label">{tabLabel}</span>
                    {session?.agent === "Codex" && hasCodexNotices ? (
                      <span
                        className="pane-tab-notice-badge"
                        aria-hidden="true"
                        title={formatCodexNoticeBadgeLabel(codexNotices.length)}
                      >
                        {codexNotices.length}
                      </span>
                    ) : null}
                  </span>
                </span>
                {tab.kind === "controlPanel" ? null : (
                  <button
                    className="pane-tab-close"
                    type="button"
                    draggable={false}
                    aria-label={`Remove ${tabLabel} from this tile`}
                    onMouseDown={(event) => {
                      event.stopPropagation();
                    }}
                    onClick={(event) => {
                      event.stopPropagation();
                      onCloseTab(paneId, tab.id);
                    }}
                  >
                    &times;
                  </button>
                )}
              </div>
            );
          })
        ) : (
          <div className="pane-empty-label">Empty tile</div>
        )}
      </div>
      {tabs.length > 1 ? (
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
      {activeStatusTooltip && activeStatusTooltipStyle && activeStatusTooltipSession
        ? createPortal(
            <SessionTabStatusTooltip
              id={activeStatusTooltip.id}
              session={activeStatusTooltipSession}
              codexState={codexState}
              projectLookup={projectLookup}
              remoteLookup={remoteLookup}
              style={activeStatusTooltipStyle}
            />,
            document.body,
          )
        : null}
      {fileTabContextMenu && fileTabContextMenuStyle
        ? createPortal(
            <div
              ref={fileTabContextMenuRef}
              className="pane-tab-context-menu panel"
              role="menu"
              aria-label="File tab actions"
              style={fileTabContextMenuStyle}
            >
              <button
                className="pane-tab-context-menu-item"
                type="button"
                role="menuitem"
                onClick={() => void handleCopyTabPath(fileTabContextMenu.path, closeFileTabContextMenu)}
              >
                Copy Path
              </button>
              <button
                className="pane-tab-context-menu-item"
                type="button"
                role="menuitem"
                disabled={!fileTabContextMenu.relativePath}
                onClick={() =>
                  void handleCopyTabPath(fileTabContextMenu.relativePath, closeFileTabContextMenu)
                }
              >
                Copy Relative Path
              </button>
              <button
                className="pane-tab-context-menu-item pane-tab-context-menu-item-danger"
                type="button"
                role="menuitem"
                onClick={() => {
                  closeFileTabContextMenu();
                  onCloseTab(fileTabContextMenu.paneId, fileTabContextMenu.tabId);
                }}
              >
                Close
              </button>
            </div>,
            document.body,
          )
        : null}

      {gitTabContextMenu && gitTabContextMenuStyle
        ? createPortal(
            <div
              ref={gitTabContextMenuRef}
              className="pane-tab-context-menu panel"
              role="menu"
              aria-label="Git tab actions"
              style={gitTabContextMenuStyle}
            >
              <button
                className="pane-tab-context-menu-item"
                type="button"
                role="menuitem"
                disabled
              >
                {formatGitTabBranchMenuLabel(gitTabContextMenu.status, gitTabContextMenu.isLoadingStatus)}
              </button>
              <button
                className="pane-tab-context-menu-item"
                type="button"
                role="menuitem"
                disabled
              >
                {formatGitTabUpstreamMenuLabel(gitTabContextMenu.status, gitTabContextMenu.isLoadingStatus)}
              </button>
              <button
                className="pane-tab-context-menu-item"
                type="button"
                role="menuitem"
                disabled
              >
                {formatGitTabWorktreeMenuLabel(gitTabContextMenu.status, gitTabContextMenu.isLoadingStatus)}
              </button>
              {gitTabContextMenu.statusError ? (
                <button
                  className="pane-tab-context-menu-item"
                  type="button"
                  role="menuitem"
                  disabled
                >
                  {gitTabContextMenu.statusError}
                </button>
              ) : null}
              {gitTabContextMenu.statusMessage ? (
                <button
                  className="pane-tab-context-menu-item"
                  type="button"
                  role="menuitem"
                  disabled
                >
                  {gitTabContextMenu.statusMessage}
                </button>
              ) : null}
              <button
                className="pane-tab-context-menu-item"
                type="button"
                role="menuitem"
                disabled={!canSyncGitTabContextMenu(gitTabContextMenu)}
                onClick={() => void handleGitTabRepoAction("sync")}
              >
                {gitTabContextMenu.pendingAction === "sync" ? "Syncing..." : "Git Sync"}
              </button>
              <button
                className="pane-tab-context-menu-item"
                type="button"
                role="menuitem"
                disabled={!canPushGitTabContextMenu(gitTabContextMenu)}
                onClick={() => void handleGitTabRepoAction("push")}
              >
                {gitTabContextMenu.pendingAction === "push" ? "Pushing..." : "Git Push"}
              </button>
              <button
                className="pane-tab-context-menu-item pane-tab-context-menu-item-danger"
                type="button"
                role="menuitem"
                onClick={() => {
                  closeGitTabContextMenu();
                  onCloseTab(paneId, gitTabContextMenu.tabId);
                }}
              >
                Close
              </button>
            </div>,
            document.body,
          )
        : null}
    </div>
  );
}

async function handleCopyTabPath(path: string | null, onDone: () => void) {
  if (!path) {
    return;
  }

  try {
    await copyTextToClipboard(path);
  } finally {
    onDone();
  }
}

function SessionTabStatusTooltip({
  codexState,
  id,
  projectLookup,
  remoteLookup,
  session,
  style,
}: {
  codexState: CodexState;
  id: string;
  projectLookup: ReadonlyMap<string, Project>;
  remoteLookup: ReadonlyMap<string, RemoteConfig>;
  session: Session;
  style: CSSProperties;
}) {
  const rateLimits = session.agent === "Codex" ? codexState.rateLimits : null;
  const notices = session.agent === "Codex" ? (codexState.notices ?? []) : [];
  const statusRows = buildSessionTooltipRows(session, projectLookup, remoteLookup);
  const hasStatusGrid = Boolean(statusRows.length || rateLimits?.primary || rateLimits?.secondary);

  return (
    <div id={id} className="pane-tab-status-tooltip" role="tooltip" style={style}>
      <div className="pane-tab-status-header">
        <div className="activity-tooltip-label">Status</div>
        {rateLimits?.planType ? <span className="pane-tab-status-plan">{rateLimits.planType}</span> : null}
      </div>
      {hasStatusGrid ? (
        <div className="pane-tab-status-grid">
          {statusRows.map((row) => (
            <Fragment key={row.key}>
              <div className="pane-tab-status-key">{row.key}:</div>
              <div className={`pane-tab-status-value${row.mono ? " pane-tab-status-mono" : ""}`}>
                {row.value}
              </div>
            </Fragment>
          ))}
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
      ) : null}
      {notices.length > 0 ? (
        <div className={`pane-tab-status-section ${hasStatusGrid ? "" : "first"}`}>
          <div className="pane-tab-status-section-label">Notices</div>
          <div className="pane-tab-status-notice-list">
            {notices.map((notice, index) => (
              <article
                key={`${notice.kind}-${notice.code ?? "notice"}-${notice.timestamp}-${index}`}
                className={`pane-tab-status-notice is-${notice.level}`}
              >
                <div className="pane-tab-status-notice-header">
                  <strong>{notice.title}</strong>
                  <span>{notice.timestamp}</span>
                </div>
                <p>{notice.detail}</p>
              </article>
            ))}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function formatCodexNoticeBadgeLabel(count: number) {
  return `${count} Codex notice${count === 1 ? "" : "s"}`;
}

function hasSessionTabStatusTooltip(_session: Session) {
  return true;
}

function formatSessionTooltipProjectLabel(
  session: Session,
  projectLookup: ReadonlyMap<string, Project>,
) {
  const projectId = session.projectId?.trim() ?? "";
  if (!projectId) {
    return "Workspace only";
  }

  return projectLookup.get(projectId)?.name ?? "Missing project";
}

function formatSessionTooltipLocationLabel(
  session: Session,
  projectLookup: ReadonlyMap<string, Project>,
  remoteLookup: ReadonlyMap<string, RemoteConfig>,
) {
  const projectId = session.projectId?.trim() ?? "";
  if (projectId) {
    const project = projectLookup.get(projectId);
    if (!project) {
      return "Unknown (missing project)";
    }

    const remoteId = resolveProjectRemoteId(project);
    const remote = remoteLookup.get(remoteId);
    if (!remote) {
      if (isLocalRemoteId(remoteId)) {
        const localRemote = createBuiltinLocalRemote();
        return `${remoteDisplayName(localRemote, localRemote.id)} (${remoteConnectionLabel(localRemote)})`;
      }

      return `${remoteId} (missing remote)`;
    }

    return `${remoteDisplayName(remote, remoteId)} (${remoteConnectionLabel(remote)})`;
  }

  const localRemote = createBuiltinLocalRemote();
  return `${remoteDisplayName(localRemote, localRemote.id)} (${remoteConnectionLabel(localRemote)})`;
}

type SessionTooltipRow = {
  key: string;
  value: string;
  mono?: boolean;
};

function formatSessionTooltipModelRow(session: Session): SessionTooltipRow {
  const currentModel = session.model.trim();
  const modelOption = matchingSessionModelOption(session.modelOptions, session.model);

  if (modelOption?.label.trim()) {
    return {
      key: "Model",
      value: modelOption.label.trim(),
    };
  }

  if (currentModel.toLowerCase() === "auto") {
    return {
      key: "Model",
      value: "Auto",
    };
  }

  if (currentModel.toLowerCase() === "default") {
    return {
      key: "Model",
      value: "Default",
    };
  }

  return {
    key: "Model",
    value: currentModel,
    mono: true,
  };
}

function buildSessionTooltipRows(
  session: Session,
  projectLookup: ReadonlyMap<string, Project>,
  remoteLookup: ReadonlyMap<string, RemoteConfig>,
): SessionTooltipRow[] {
  const rows: SessionTooltipRow[] = [
    { key: "Agent", value: session.agent },
    { key: "State", value: formatTooltipEnumLabel(session.status) },
    { key: "Project", value: formatSessionTooltipProjectLabel(session, projectLookup) },
    { key: "Location", value: formatSessionTooltipLocationLabel(session, projectLookup, remoteLookup) },
    formatSessionTooltipModelRow(session),
  ];

  if (session.externalSessionId) {
    rows.push({
      key: "Session",
      value: session.externalSessionId,
      mono: true,
    });
  }

  if (session.approvalPolicy) {
    rows.push({
      key: "Policy",
      value: formatTooltipEnumLabel(session.approvalPolicy),
    });
  }

  if (session.agent === "Codex") {
    if (session.sandboxMode) {
      rows.push({
        key: "Sandbox",
        value: formatTooltipEnumLabel(session.sandboxMode),
      });
    }
    if (session.reasoningEffort) {
      rows.push({
        key: "Reasoning",
        value: formatTooltipEnumLabel(session.reasoningEffort),
      });
    }
    if (session.codexThreadState) {
      rows.push({
        key: "Thread",
        value: formatTooltipEnumLabel(session.codexThreadState),
      });
    }
  }

  if (session.agent === "Claude") {
    if (session.claudeApprovalMode) {
      rows.push({
        key: "Approval",
        value: formatTooltipEnumLabel(session.claudeApprovalMode),
      });
    }
    if (session.claudeEffort) {
      rows.push({
        key: "Effort",
        value: formatTooltipEnumLabel(session.claudeEffort),
      });
    }
  }

  if (session.agent === "Cursor" && session.cursorMode) {
    rows.push({
      key: "Mode",
      value: formatTooltipEnumLabel(session.cursorMode),
    });
  }

  if (session.agent === "Gemini" && session.geminiApprovalMode) {
    rows.push({
      key: "Approval",
      value: formatTooltipEnumLabel(session.geminiApprovalMode),
    });
  }

  return rows;
}

function formatTooltipEnumLabel(value: string) {
  if (value === "xhigh") {
    return "XHigh";
  }

  if (value === "yolo") {
    return "YOLO";
  }

  return value
    .split(/[-_]/)
    .map((part) => (part ? `${part.charAt(0).toUpperCase()}${part.slice(1)}` : part))
    .join(" ");
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

function formatTabLabel(tab: WorkspaceTab, session: Session | null) {
  if (tab.kind === "session") {
    return session?.name ?? tab.sessionId;
  }

  if (tab.kind === "source") {
    return formatPathTabLabel(tab.path, "Open file");
  }

  if (tab.kind === "filesystem") {
    return `Files: ${formatPathTabLabel(tab.rootPath, "Workspace")}`;
  }

  if (tab.kind === "gitStatus") {
    return `Git: ${formatPathTabLabel(tab.workdir, "Workspace")}`;
  }

  if (tab.kind === "controlPanel") {
    return "Control panel";
  }

  if (tab.kind === "orchestratorList") {
    return "Orchestrators";
  }

  if (tab.kind === "canvas") {
    return "Canvas";
  }

  if (tab.kind === "orchestratorCanvas") {
    return tab.templateId ? `Orchestration: ${tab.templateId}` : "New orchestration";
  }

  if (tab.kind === "sessionList") {
    return "Sessions";
  }

  if (tab.kind === "projectList") {
    return "Projects";
  }

  if (tab.kind === "instructionDebugger") {
    return `Instructions: ${formatPathTabLabel(tab.workdir, "Workspace")}`;
  }

  return `Diff: ${formatPathTabLabel(tab.filePath, "Preview")}`;
}

function formatPathTabLabel(path: string | null, fallback: string) {
  const trimmed = path?.trim();
  if (!trimmed) {
    return fallback;
  }

  const segments = trimmed.split(/[/\\]+/).filter(Boolean);
  if (segments.length === 0) {
    return trimmed;
  }

  return segments[segments.length - 1] ?? trimmed;
}

function formatRateLimitResetLabel(resetsAt: number | null, label: string) {
  if (!resetsAt) {
    return null;
  }

  const date = new Date(resetsAt * 1000);
  if (Number.isNaN(date.getTime())) {
    return null;
  }

  const now = new Date();
  const sameDay = date.toDateString() === now.toDateString();
  const formatter = sameDay
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

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}


function formatGitTabContextMenuError(error: unknown) {
  if (error instanceof Error) {
    const message = error.message.trim();
    if (message) {
      return message;
    }
  }

  return "Git action failed.";
}

function formatGitTabBranchMenuLabel(
  status: GitStatusResponse | null,
  isLoadingStatus: boolean,
) {
  if (isLoadingStatus && !status) {
    return "Branch: Loading...";
  }

  if (!status) {
    return "Branch: Unavailable";
  }

  const branch = status.branch?.trim();
  return `Branch: ${branch || "Detached HEAD"}`;
}

function formatGitTabUpstreamMenuLabel(
  status: GitStatusResponse | null,
  isLoadingStatus: boolean,
) {
  if (isLoadingStatus && !status) {
    return "Upstream: Loading...";
  }

  if (!status) {
    return "Upstream: Unavailable";
  }

  const upstream = status.upstream?.trim();
  if (!upstream) {
    return "Upstream: Not tracking";
  }

  const position: string[] = [];
  if (status.ahead > 0) {
    position.push(`ahead ${status.ahead}`);
  }
  if (status.behind > 0) {
    position.push(`behind ${status.behind}`);
  }

  const suffix = position.length > 0 ? ` (${position.join(", ")})` : " (up to date)";
  return `Upstream: ${upstream}${suffix}`;
}

function formatGitTabWorktreeMenuLabel(
  status: GitStatusResponse | null,
  isLoadingStatus: boolean,
) {
  if (isLoadingStatus && !status) {
    return "Status: Loading...";
  }

  if (!status) {
    return "Status: Unavailable";
  }

  if (status.isClean) {
    return "Status: Clean";
  }

  const count = status.files.length;
  return `Status: ${count} changed ${count === 1 ? "file" : "files"}`;
}

function canPushGitTabContextMenu(menu: GitTabContextMenuState | null) {
  return Boolean(
    menu &&
      !menu.isLoadingStatus &&
      !menu.pendingAction &&
      menu.status?.branch?.trim(),
  );
}

function canSyncGitTabContextMenu(menu: GitTabContextMenuState | null) {
  return Boolean(
    menu &&
      !menu.isLoadingStatus &&
      !menu.pendingAction &&
      menu.status?.upstream?.trim(),
  );
}

function buildGitTabContextMenu(
  tab: WorkspaceTab,
  sessionLookup: ReadonlyMap<string, Session>,
  projectLookup: ReadonlyMap<string, Project>,
) {
  if (tab.kind !== "gitStatus") {
    return null;
  }

  const workdir = tab.workdir?.trim() || resolveGitTabWorkspaceRoot(tab, sessionLookup, projectLookup);
  if (!workdir) {
    return null;
  }

  const originSession =
    tab.originSessionId ? (sessionLookup.get(tab.originSessionId) ?? null) : null;
  const projectId = tab.originProjectId ?? originSession?.projectId ?? null;
  return {
    projectId,
    sessionId: tab.originSessionId ?? null,
    workdir,
  };
}

function resolveGitTabWorkspaceRoot(
  tab: WorkspaceGitStatusTab,
  sessionLookup: ReadonlyMap<string, Session>,
  projectLookup: ReadonlyMap<string, Project>,
) {
  const originSession =
    tab.originSessionId ? (sessionLookup.get(tab.originSessionId) ?? null) : null;
  if (originSession?.workdir) {
    return originSession.workdir;
  }

  const originProjectId = tab.originProjectId ?? originSession?.projectId ?? null;
  return originProjectId ? (projectLookup.get(originProjectId)?.rootPath ?? null) : null;
}

function buildFileTabContextMenu(
  tab: WorkspaceTab,
  sessionLookup: ReadonlyMap<string, Session>,
  projectLookup: ReadonlyMap<string, Project>,
) {
  const path = getFileTabPath(tab);
  if (!path) {
    return null;
  }

  const workspaceRoot = resolveFileTabWorkspaceRoot(tab, sessionLookup, projectLookup);
  return {
    path,
    relativePath: resolveRelativeTabPath(path, workspaceRoot),
  };
}

function getFileTabPath(tab: WorkspaceTab) {
  if (tab.kind === "source") {
    return tab.path?.trim() || null;
  }

  if (tab.kind === "diffPreview") {
    return tab.filePath?.trim() || null;
  }

  return null;
}

function resolveFileTabWorkspaceRoot(
  tab: WorkspaceTab,
  sessionLookup: ReadonlyMap<string, Session>,
  projectLookup: ReadonlyMap<string, Project>,
) {
  if (tab.kind !== "source" && tab.kind !== "diffPreview") {
    return null;
  }

  const originSession =
    tab.originSessionId ? (sessionLookup.get(tab.originSessionId) ?? null) : null;
  if (originSession?.workdir) {
    return originSession.workdir;
  }

  const originProjectId = tab.originProjectId ?? originSession?.projectId ?? null;
  return originProjectId ? (projectLookup.get(originProjectId)?.rootPath ?? null) : null;
}

function resolveRelativeTabPath(path: string, workspaceRoot: string | null) {
  const trimmedPath = path.trim();
  if (!trimmedPath) {
    return null;
  }

  if (!looksLikeAbsoluteDisplayPath(trimmedPath)) {
    return normalizeDisplayPath(trimmedPath);
  }

  if (!workspaceRoot) {
    return null;
  }

  const relativePath = relativizePathToWorkspace(trimmedPath, workspaceRoot);
  return relativePath === trimmedPath ? null : relativePath;
}
