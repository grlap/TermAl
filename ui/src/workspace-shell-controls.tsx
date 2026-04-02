import { useMemo, type RefObject } from "react";

import type { WorkspaceLayoutSummary } from "./api";

export function WorkspaceSwitcher({
  currentWorkspaceId,
  error,
  isLoading,
  isOpen,
  summaries,
  switcherRef,
  onOpenNewWorkspaceHere,
  onOpenNewWorkspaceWindow,
  onOpenWorkspace,
  onToggle,
}: {
  currentWorkspaceId: string;
  error: string | null;
  isLoading: boolean;
  isOpen: boolean;
  summaries: readonly WorkspaceLayoutSummary[];
  switcherRef: RefObject<HTMLDivElement>;
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
              return (
                <button
                  key={summary.id}
                  className={`workspace-switcher-item ${isCurrent ? "selected" : ""}`}
                  type="button"
                  role="listitem"
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

function describeBackendConnectionState(state: BackendConnectionState) {
  switch (state) {
    case "connecting":
      return {
        detail: "Connecting to the TermAl backend.",
        icon: "spinner" as const,
        label: "Connecting",
        tone: "active" as const,
      };
    case "connected":
      return {
        detail: "Live updates are connected.",
        icon: "connected" as const,
        label: "Connected",
        tone: "idle" as const,
      };
    case "reconnecting":
      return {
        detail: "Live updates are disconnected. Trying to reconnect.",
        icon: "spinner" as const,
        label: "Reconnecting",
        tone: "active" as const,
      };
    case "offline":
      return {
        detail: "The browser is offline or cannot reach the backend.",
        icon: "offline" as const,
        label: "Offline",
        tone: "error" as const,
      };
  }
}

export function BackendConnectionStatus({ state }: { state: BackendConnectionState }) {
  const descriptor = describeBackendConnectionState(state);

  return (
    <div className="workspace-connection-status" role="status" aria-live="polite" title={descriptor.detail}>
      <span className={`chip chip-status chip-status-${descriptor.tone} workspace-connection-chip`}>
        {descriptor.icon === "spinner" ? (
          <span className="activity-spinner workspace-connection-spinner" aria-hidden="true" />
        ) : (
          <BackendConnectionIcon state={descriptor.icon} />
        )}
        <span className="visually-hidden">{descriptor.label}</span>
      </span>
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