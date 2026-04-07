import { useId, useMemo, useState, type RefObject } from "react";

import type { WorkspaceLayoutSummary } from "./api";

export function WorkspaceSwitcher({
  currentWorkspaceId,
  deletingWorkspaceIds,
  error,
  isLoading,
  isOpen,
  summaries,
  switcherRef,
  onDeleteWorkspace,
  onOpenNewWorkspaceHere,
  onOpenNewWorkspaceWindow,
  onOpenWorkspace,
  onToggle,
}: {
  currentWorkspaceId: string;
  deletingWorkspaceIds: readonly string[];
  error: string | null;
  isLoading: boolean;
  isOpen: boolean;
  summaries: readonly WorkspaceLayoutSummary[];
  switcherRef: RefObject<HTMLDivElement>;
  onDeleteWorkspace: (workspaceId: string) => void;
  onOpenNewWorkspaceHere: () => void;
  onOpenNewWorkspaceWindow: () => void;
  onOpenWorkspace: (workspaceId: string) => void;
  onToggle: () => void;
}) {
  const visibleSummaries = useMemo(() => {
    const byId = new Map(summaries.map((summary) => [summary.id, summary]));
    if (!byId.has(currentWorkspaceId)) {
      byId.set(currentWorkspaceId, {
        id: currentWorkspaceId,
        revision: 0,
        updatedAt: "Current browser view",
        controlPanelSide: "left",
      });
    }

    return [...byId.values()];
  }, [currentWorkspaceId, summaries]);
  const deletingWorkspaceIdSet = useMemo(
    () => new Set(deletingWorkspaceIds),
    [deletingWorkspaceIds],
  );

  return (
    <div ref={switcherRef} className="workspace-switcher">
      <button
        className={`ghost-button workspace-switcher-trigger ${isOpen ? "open" : ""}`}
        type="button"
        aria-expanded={isOpen}
        aria-haspopup="dialog"
        aria-label={`Workspace ${currentWorkspaceId}`}
        onClick={onToggle}
      >
        <span className="workspace-switcher-trigger-copy">
          <span className="workspace-switcher-trigger-label">Workspace</span>
          <span className="workspace-switcher-trigger-value">
            {formatWorkspaceSwitcherLabel(currentWorkspaceId)}
          </span>
        </span>
        <span className={`combo-trigger-caret ${isOpen ? "open" : ""}`} aria-hidden="true">
          v
        </span>
      </button>

      {isOpen ? (
        <div className="workspace-switcher-menu panel" role="dialog" aria-label="Workspace switcher">
          <div className="workspace-switcher-menu-header">
            <div>
              <div className="card-label">Workspace</div>
              <h3>Switch browser layout</h3>
            </div>
            <span className="workspace-switcher-current-id" title={currentWorkspaceId}>
              {currentWorkspaceId}
            </span>
          </div>

          <div className="workspace-switcher-actions">
            <button className="ghost-button" type="button" onClick={onOpenNewWorkspaceHere}>
              New here
            </button>
            <button className="ghost-button" type="button" onClick={onOpenNewWorkspaceWindow}>
              New window
            </button>
          </div>

          <div className="workspace-switcher-list" role="list">
            {visibleSummaries.map((summary) => {
              const isCurrent = summary.id === currentWorkspaceId;
              const isDeleting = deletingWorkspaceIdSet.has(summary.id);
              return (
                <div
                  key={summary.id}
                  className="workspace-switcher-item-shell"
                  role="listitem"
                >
                  <button
                    className={`workspace-switcher-item ${isCurrent ? "selected" : ""}`}
                    type="button"
                    onClick={() => onOpenWorkspace(summary.id)}
                  >
                    <span className="workspace-switcher-item-copy">
                      <span className="workspace-switcher-item-title-row">
                        <span className="workspace-switcher-item-title">
                          {formatWorkspaceSwitcherLabel(summary.id)}
                        </span>
                        {isCurrent ? (
                          <span className="workspace-switcher-item-status">Current</span>
                        ) : null}
                      </span>
                      <span className="workspace-switcher-item-meta" title={summary.id}>
                        {summary.id}
                      </span>
                      <span className="workspace-switcher-item-meta">
                        {summary.updatedAt}
                      </span>
                    </span>
                  </button>
                  {isCurrent ? null : (
                    <button
                      className="ghost-button workspace-switcher-item-delete"
                      type="button"
                      onClick={(event) => {
                        event.stopPropagation();
                        onDeleteWorkspace(summary.id);
                      }}
                      disabled={isDeleting}
                      aria-label={`Delete workspace ${summary.id}`}
                    >
                      {isDeleting ? "Deleting" : "Delete"}
                    </button>
                  )}
                </div>
              );
            })}
          </div>

          {isLoading ? (
            <p className="workspace-switcher-status">Loading saved workspaces\u2026</p>
          ) : null}
          {error ? <p className="workspace-switcher-error">{error}</p> : null}
        </div>
      ) : null}
    </div>
  );
}

function formatWorkspaceSwitcherLabel(workspaceId: string) {
  const normalized = workspaceId.trim();
  if (normalized.startsWith("workspace-") && normalized.length > "workspace-".length + 8) {
    return normalized.slice("workspace-".length, "workspace-".length + 8);
  }

  return normalized;
}

type BackendConnectionState = "connecting" | "connected" | "reconnecting" | "offline";

type BackendConnectionDescriptor = {
  detail: string;
  icon: "spinner" | "connected" | "offline";
  label: string;
  tone: "active" | "idle" | "error";
};

function describeBackendConnectionState(
  state: BackendConnectionState,
): BackendConnectionDescriptor {
  switch (state) {
    case "connecting":
      return {
        detail: "Connecting to the TermAl backend.",
        icon: "spinner",
        label: "Connecting",
        tone: "active",
      };
    case "connected":
      return {
        detail: "Live updates are connected.",
        icon: "connected",
        label: "Connected",
        tone: "idle",
      };
    case "reconnecting":
      return {
        detail: "Live updates are disconnected. Trying to reconnect.",
        icon: "spinner",
        label: "Reconnecting",
        tone: "error",
      };
    case "offline":
      return {
        detail: "The browser is offline or cannot reach the backend.",
        icon: "offline",
        label: "Offline",
        tone: "error",
      };
  }
}

export function ControlPanelConnectionIndicator({
  issueDetail = null,
  state,
}: {
  issueDetail?: string | null;
  state: BackendConnectionState;
}) {
  const descriptor = describeBackendConnectionState(state);
  const tooltipId = useId();
  const [isTooltipVisible, setIsTooltipVisible] = useState(false);
  const detail = issueDetail ?? (state === "connected" ? null : descriptor.detail);
  if (detail === null || (state === "connecting" && issueDetail === null)) {
    return null;
  }

  const showSpinner = descriptor.icon === "spinner" || issueDetail !== null;
  const showGenericIssueLabel = issueDetail !== null && state === "connected";
  const displayLabel = showGenericIssueLabel ? "Issue" : descriptor.label;
  const ariaLabel = showGenericIssueLabel
    ? "Control panel issue"
    : `Control panel backend ${descriptor.label.toLowerCase()}`;

  return (
    <div
      className="control-panel-pane-status-shell"
      onBlur={(event) => {
        const nextTarget = event.relatedTarget;
        if (nextTarget instanceof Node && event.currentTarget.contains(nextTarget)) {
          return;
        }
        setIsTooltipVisible(false);
      }}
      onFocus={() => {
        setIsTooltipVisible(true);
      }}
      onMouseEnter={() => {
        setIsTooltipVisible(true);
      }}
      onMouseLeave={() => {
        setIsTooltipVisible(false);
      }}
    >
      <span
        className="control-panel-pane-status"
        role="img"
        aria-label={ariaLabel}
        aria-describedby={isTooltipVisible ? tooltipId : undefined}
        tabIndex={0}
      >
        {showSpinner ? (
          <span
            className="activity-spinner control-panel-pane-status-spinner"
            aria-hidden="true"
          />
        ) : (
          <span className="control-panel-pane-status-icon" aria-hidden="true">
            <BackendConnectionIcon state="offline" />
          </span>
        )}
      </span>
      <div
        id={tooltipId}
        className="control-panel-pane-status-tooltip"
        role="tooltip"
        aria-hidden={isTooltipVisible ? undefined : true}
      >
        <div className="control-panel-pane-status-tooltip-label">{displayLabel}</div>
        <div className="control-panel-pane-status-tooltip-detail">{detail}</div>
      </div>
    </div>
  );
}

export function BackendConnectionStatus({
  issueDetail = null,
  state,
}: {
  issueDetail?: string | null;
  state: BackendConnectionState;
}) {
  const descriptor = describeBackendConnectionState(state);
  const tooltipId = useId();
  const [isTooltipVisible, setIsTooltipVisible] = useState(false);
  const hasIssue = descriptor.tone === "error" || issueDetail !== null;
  const tooltipDetail = issueDetail ?? (state === "connected" ? null : descriptor.detail);
  const hasTooltip = tooltipDetail !== null;

  return (
    <div
      className={`workspace-connection-status ${hasIssue ? "has-issue" : ""} ${hasTooltip ? "has-tooltip" : ""}`.trim()}
      role="status"
      aria-live="polite"
      onBlur={(event) => {
        const nextTarget = event.relatedTarget;
        if (nextTarget instanceof Node && event.currentTarget.contains(nextTarget)) {
          return;
        }
        setIsTooltipVisible(false);
      }}
      onFocus={() => {
        if (hasTooltip) {
          setIsTooltipVisible(true);
        }
      }}
      onMouseEnter={() => {
        if (hasTooltip) {
          setIsTooltipVisible(true);
        }
      }}
      onMouseLeave={() => {
        if (hasTooltip) {
          setIsTooltipVisible(false);
        }
      }}
    >
      <span
        className={`chip chip-status chip-status-${hasIssue ? "error" : descriptor.tone} workspace-connection-chip`}
        aria-describedby={hasTooltip && isTooltipVisible ? tooltipId : undefined}
        aria-label={descriptor.label}
        tabIndex={hasTooltip ? 0 : undefined}
      >
        {descriptor.icon === "spinner" ? (
          <span className="activity-spinner workspace-connection-spinner" aria-hidden="true" />
        ) : (
          <BackendConnectionIcon state={descriptor.icon} />
        )}
        <span className="visually-hidden">{descriptor.label}</span>
      </span>
      {hasTooltip ? (
        <div
          id={tooltipId}
          className="activity-tooltip workspace-connection-tooltip"
          role="tooltip"
          aria-hidden={isTooltipVisible ? undefined : true}
        >
          <div className="activity-tooltip-label">{descriptor.label}</div>
          <p>{tooltipDetail}</p>
        </div>
      ) : null}
    </div>
  );
}
function BackendConnectionIcon({ state }: { state: "connected" | "offline" }) {
  if (state === "connected") {
    return (
      <span className="workspace-connection-icon" aria-hidden="true">
        <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
          <path
            d="m4 8.2 2.2 2.2L12 4.6"
            fill="none"
            stroke="currentColor"
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth="1.8"
          />
        </svg>
      </span>
    );
  }

  return (
    <span className="workspace-connection-icon" aria-hidden="true">
      <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
        <path
          d="M4.5 4.5 11.5 11.5"
          fill="none"
          stroke="currentColor"
          strokeLinecap="round"
          strokeWidth="1.8"
        />
        <path
          d="M11.5 4.5 4.5 11.5"
          fill="none"
          stroke="currentColor"
          strokeLinecap="round"
          strokeWidth="1.8"
        />
      </svg>
    </span>
  );
}
