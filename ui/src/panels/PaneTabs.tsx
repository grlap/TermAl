import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type DragEvent as ReactDragEvent,
} from "react";
import { createPortal } from "react-dom";
import { AgentIcon } from "../agent-icon";
import { copyTextToClipboard } from "../clipboard";
import { FileTabIcon } from "../file-tab-icon";
import {
  looksLikeAbsoluteDisplayPath,
  normalizeDisplayPath,
  relativizePathToWorkspace,
} from "../path-display";
import {
  attachWorkspaceTabDragData,
  createWorkspaceTabDrag,
  type WorkspaceTabDrag,
} from "../tab-drag";
import { measurePaneTabStatusTooltipPosition } from "../pane-tab-status-tooltip";
import type { CodexRateLimitWindow, CodexState, Project, Session } from "../types";
import type { TabDropPlacement, WorkspaceTab } from "../workspace";

type ActiveCodexTooltipState = {
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

export function PaneTabs({
  paneId,
  windowId,
  tabs,
  activeTabId,
  codexState,
  projectLookup,
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
  const activeCodexTooltipAnchorRef = useRef<HTMLElement | null>(null);
  const fileTabContextMenuRef = useRef<HTMLDivElement | null>(null);
  const paneHasControlPanel = tabs.some((tab) => tab.kind === "controlPanel");
  const canDropInTabRail = draggedTab !== null &&
    draggedTab.tab.kind !== "controlPanel" &&
    !paneHasControlPanel;
  const [tabRailState, setTabRailState] = useState({
    hasOverflow: false,
    canScrollPrev: false,
    canScrollNext: false,
  });
  const [activeTabInsertIndex, setActiveTabInsertIndex] = useState<number | null>(null);
  const [activeCodexTooltip, setActiveCodexTooltip] = useState<ActiveCodexTooltipState | null>(null);
  const [activeCodexTooltipStyle, setActiveCodexTooltipStyle] = useState<CSSProperties | null>(null);
  const [fileTabContextMenu, setFileTabContextMenu] = useState<FileTabContextMenuState | null>(null);
  const [fileTabContextMenuStyle, setFileTabContextMenuStyle] = useState<CSSProperties | null>(null);

  function updateActiveCodexTooltipPosition(anchor = activeCodexTooltipAnchorRef.current) {
    if (!anchor || typeof window === "undefined") {
      setActiveCodexTooltipStyle(null);
      return;
    }

    const position = measurePaneTabStatusTooltipPosition(anchor.getBoundingClientRect(), window.innerWidth);
    const nextStyle = {
      left: `${position.left}px`,
      top: `${position.top}px`,
      width: `${position.width}px`,
      ["--pane-tab-status-arrow-left"]: `${position.arrowLeft}px`,
    } as CSSProperties;
    setActiveCodexTooltipStyle(nextStyle);
  }

  function openCodexStatusTooltip(id: string, sessionId: string, anchor: HTMLElement) {
    activeCodexTooltipAnchorRef.current = anchor;
    setActiveCodexTooltip((current) =>
      current?.id === id && current.sessionId === sessionId ? current : { id, sessionId },
    );
    updateActiveCodexTooltipPosition(anchor);
  }

  function closeCodexStatusTooltip() {
    activeCodexTooltipAnchorRef.current = null;
    setActiveCodexTooltip(null);
    setActiveCodexTooltipStyle(null);
  }

  function closeFileTabContextMenu() {
    setFileTabContextMenu(null);
    setFileTabContextMenuStyle(null);
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
    if (!fileTabContextMenu) {
      return;
    }

    if (!tabs.some((tab) => tab.id === fileTabContextMenu.tabId)) {
      closeFileTabContextMenu();
    }
  }, [fileTabContextMenu, tabs]);

  useLayoutEffect(() => {
    if (!activeCodexTooltip) {
      return;
    }

    const updateTooltipPosition = () => {
      const anchor = activeCodexTooltipAnchorRef.current;
      if (!anchor || !anchor.isConnected) {
        closeCodexStatusTooltip();
        return;
      }

      updateActiveCodexTooltipPosition(anchor);
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
  }, [activeCodexTooltip, activeTabId, tabs.length]);

  useLayoutEffect(() => {
    if (!fileTabContextMenu) {
      return;
    }

    updateFileTabContextMenuPosition();
  }, [fileTabContextMenu]);

  useEffect(() => {
    if (!activeCodexTooltip || sessionLookup.has(activeCodexTooltip.sessionId)) {
      return;
    }

    closeCodexStatusTooltip();
  }, [activeCodexTooltip, sessionLookup]);

  const activeCodexTooltipSession = activeCodexTooltip
    ? (sessionLookup.get(activeCodexTooltip.sessionId) ?? null)
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
      >
        {tabs.length > 0 ? (
          tabs.map((tab, index) => {
            const session = tab.kind === "session" ? (sessionLookup.get(tab.sessionId) ?? null) : null;
            const tabActive = tab.id === activeTabId;
            const showCodexStatus = Boolean(
              session &&
                session.agent === "Codex" &&
                (session.externalSessionId || codexState.rateLimits),
            );
            const showDropBefore = activeTabInsertIndex === index;
            const showDropAfter = activeTabInsertIndex === tabs.length && index === tabs.length - 1;
            const codexStatusTooltipId = showCodexStatus ? `codex-status-${paneId}-${tab.id}` : undefined;
            const tabLabel = formatTabLabel(tab, session);

            return (
              <div
                key={tab.id}
                className={`pane-tab-shell ${tabActive ? "active" : ""} ${showCodexStatus ? "has-status-tooltip" : ""} ${showDropBefore ? "drop-before" : ""} ${showDropAfter ? "drop-after" : ""}`}
                role="tab"
                aria-selected={tabActive}
                aria-describedby={activeCodexTooltip?.id === codexStatusTooltipId ? codexStatusTooltipId : undefined}
                tabIndex={0}
                onMouseEnter={(event) => {
                  if (showCodexStatus && session && codexStatusTooltipId) {
                    openCodexStatusTooltip(codexStatusTooltipId, session.id, event.currentTarget);
                  }
                }}
                onMouseLeave={() => {
                  closeCodexStatusTooltip();
                }}
                onFocus={(event) => {
                  if (showCodexStatus && session && codexStatusTooltipId) {
                    openCodexStatusTooltip(codexStatusTooltipId, session.id, event.currentTarget);
                  }
                }}
                onBlur={(event) => {
                  const nextTarget = event.relatedTarget;
                  if (nextTarget instanceof Node && event.currentTarget.contains(nextTarget)) {
                    return;
                  }

                  closeCodexStatusTooltip();
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
      {activeCodexTooltip && activeCodexTooltipStyle && activeCodexTooltipSession
        ? createPortal(
            <CodexTabStatusTooltip
              id={activeCodexTooltip.id}
              session={activeCodexTooltipSession}
              codexState={codexState}
              style={activeCodexTooltipStyle}
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

function CodexTabStatusTooltip({
  codexState,
  id,
  session,
  style,
}: {
  codexState: CodexState;
  id: string;
  session: Session;
  style: CSSProperties;
}) {
  const rateLimits = codexState.rateLimits;

  return (
    <div id={id} className="pane-tab-status-tooltip" role="tooltip" style={style}>
      <div className="pane-tab-status-header">
        <div className="activity-tooltip-label">Status</div>
        {rateLimits?.planType ? <span className="pane-tab-status-plan">{rateLimits.planType}</span> : null}
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
