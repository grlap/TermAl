import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type DragEvent as ReactDragEvent,
} from "react";
import type { CodexRateLimitWindow, CodexState, Session } from "../types";
import type { TabDropPlacement, WorkspaceTab } from "../workspace";

export function PaneTabs({
  paneId,
  tabs,
  activeTabId,
  codexState,
  sessionLookup,
  draggedTab,
  onSelectTab,
  onCloseTab,
  onTabDragStart,
  onTabDragEnd,
  onTabDrop,
}: {
  paneId: string;
  tabs: WorkspaceTab[];
  activeTabId: string | null;
  codexState: CodexState;
  sessionLookup: Map<string, Session>;
  draggedTab: {
    sourcePaneId: string;
    tabId: string;
  } | null;
  onSelectTab: (paneId: string, tabId: string) => void;
  onCloseTab: (paneId: string, tabId: string) => void;
  onTabDragStart: (sourcePaneId: string, tabId: string) => void;
  onTabDragEnd: () => void;
  onTabDrop: (targetPaneId: string, placement: TabDropPlacement, tabIndex?: number) => void;
}) {
  const paneTabsRef = useRef<HTMLDivElement | null>(null);
  const [tabRailState, setTabRailState] = useState({
    hasOverflow: false,
    canScrollPrev: false,
    canScrollNext: false,
  });
  const [activeTabInsertIndex, setActiveTabInsertIndex] = useState<number | null>(null);

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
    if (!draggedTab) {
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
    if (!draggedTab) {
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
      activeTab?.scrollIntoView({
        block: "nearest",
        inline: "nearest",
      });
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
                aria-describedby={codexStatusTooltipId}
                tabIndex={0}
                onClick={() => onSelectTab(paneId, tab.id)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    onSelectTab(paneId, tab.id);
                  }
                }}
              >
                <button
                  className="pane-tab-grip"
                  type="button"
                  aria-label={`Drag ${tabLabel}`}
                  draggable
                  onMouseDown={(event) => {
                    event.stopPropagation();
                  }}
                  onDragStart={(event) => {
                    event.dataTransfer.effectAllowed = "move";
                    event.dataTransfer.setData("text/plain", tab.id);
                    const tabShell = event.currentTarget.closest(".pane-tab-shell");
                    if (tabShell instanceof HTMLElement) {
                      const rect = tabShell.getBoundingClientRect();
                      event.dataTransfer.setDragImage(
                        tabShell,
                        Math.max(12, event.clientX - rect.left),
                        Math.max(12, event.clientY - rect.top),
                      );
                    }
                    onTabDragStart(paneId, tab.id);
                  }}
                  onDragEnd={onTabDragEnd}
                />
                <span className="pane-tab">
                  <span className="pane-tab-copy">
                    {session ? <span className={`status-dot status-${session.status}`} /> : null}
                    <span className="pane-tab-label">{tabLabel}</span>
                  </span>
                </span>
                {showCodexStatus && session ? (
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
                  aria-label={`Remove ${tabLabel} from this tile`}
                  onMouseDown={(event) => {
                    event.stopPropagation();
                  }}
                  onClick={(event) => {
                    event.stopPropagation();
                    onCloseTab(paneId, tab.id);
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
    </div>
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
    return `${session?.emoji ?? ""} ${session?.name ?? tab.sessionId}`.trim();
  }

  if (tab.kind === "source") {
    return formatPathTabLabel(tab.path, "Open file");
  }

  if (tab.kind === "filesystem") {
    return `Files: ${formatPathTabLabel(tab.rootPath, "Workspace")}`;
  }

  return `Git: ${formatPathTabLabel(tab.workdir, "Workspace")}`;
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
