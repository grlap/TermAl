import { useId, useState } from "react";

import { primaryModifierLabel } from "./app-utils";
import type { ThemeKind } from "./themes";

export function ThemeModeToggle({
  effectiveThemeKind,
  hasAutoOverride,
  onToggle,
}: {
  effectiveThemeKind: ThemeKind;
  hasAutoOverride: boolean;
  onToggle: () => void;
}) {
  const nextKind = effectiveThemeKind === "dark" ? "light" : "dark";
  const shortcut = `${primaryModifierLabel()}+Shift+L`;
  const overrideCopy = hasAutoOverride
    ? " Auto is overridden for this session; return to Auto in Themes settings."
    : "";

  return (
    <button
      className={`ghost-button theme-mode-toggle ${hasAutoOverride ? "overridden" : ""}`.trim()}
      type="button"
      aria-label={`Switch to ${nextKind} theme`}
      title={`Switch to ${nextKind} theme (${shortcut}).${overrideCopy}`}
      onClick={onToggle}
    >
      <span aria-hidden="true">{effectiveThemeKind === "dark" ? "☀︎" : "☾"}</span>
      {hasAutoOverride ? (
        <span className="theme-mode-toggle-override-dot" aria-hidden="true" />
      ) : null}
    </button>
  );
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
        detail: "Live updates are disconnected. Retrying automatically with backoff.",
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
  onRetry = null,
  state,
}: {
  issueDetail?: string | null;
  onRetry?: (() => void) | null;
  state: BackendConnectionState;
}) {
  const descriptor = describeBackendConnectionState(state);
  const tooltipId = useId();
  const [isTooltipVisible, setIsTooltipVisible] = useState(false);
  const detail = issueDetail ?? (state === "connected" ? null : descriptor.detail);
  if (detail === null || (state === "connecting" && issueDetail === null)) {
    return null;
  }

  const canRetry = onRetry !== null && (state === "connecting" || state === "reconnecting");
  const showSpinner = descriptor.icon === "spinner" || issueDetail !== null;
  const showGenericIssueLabel = issueDetail !== null && state === "connected";
  const displayLabel = showGenericIssueLabel ? "Issue" : descriptor.label;
  const ariaLabel = showGenericIssueLabel
    ? "Control panel issue"
    : `Control panel backend ${descriptor.label.toLowerCase()}`;

  return (
    <div
      className={`control-panel-pane-status-shell ${canRetry ? "is-actionable" : ""}`.trim()}
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
      {canRetry ? (
        <button
          className="control-panel-pane-status is-actionable"
          type="button"
          onClick={() => {
            onRetry?.();
          }}
          aria-label={ariaLabel}
          aria-describedby={isTooltipVisible ? tooltipId : undefined}
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
        </button>
      ) : (
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
      )}
      <div
        id={tooltipId}
        className="control-panel-pane-status-tooltip"
        role="tooltip"
        aria-hidden={isTooltipVisible ? undefined : true}
      >
        <div className="control-panel-pane-status-tooltip-label">{displayLabel}</div>
        <div className="control-panel-pane-status-tooltip-detail">{detail}</div>
        {canRetry ? (
          <div className="control-panel-pane-status-tooltip-action">
            Click the status to retry now.
          </div>
        ) : null}
      </div>
    </div>
  );
}

export function BackendConnectionStatus({
  issueDetail = null,
  onRetry = null,
  state,
}: {
  issueDetail?: string | null;
  onRetry?: (() => void) | null;
  state: BackendConnectionState;
}) {
  const descriptor = describeBackendConnectionState(state);
  const tooltipId = useId();
  const [isTooltipVisible, setIsTooltipVisible] = useState(false);
  const hasIssue = descriptor.tone === "error" || issueDetail !== null;
  const tooltipDetail = issueDetail ?? (state === "connected" ? null : descriptor.detail);
  const hasTooltip = tooltipDetail !== null;
  const canRetry = onRetry !== null && (state === "connecting" || state === "reconnecting");

  return (
    <div
      className={`workspace-connection-status ${hasIssue ? "has-issue" : ""} ${hasTooltip ? "has-tooltip" : ""} ${canRetry ? "is-actionable" : ""}`.trim()}
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
      {canRetry ? (
        <button
          className={`chip chip-status chip-status-${hasIssue ? "error" : descriptor.tone} workspace-connection-chip is-actionable`}
          type="button"
          onClick={() => {
            onRetry?.();
          }}
          aria-describedby={hasTooltip && isTooltipVisible ? tooltipId : undefined}
          aria-label={descriptor.label}
        >
          {descriptor.icon === "spinner" ? (
            <span className="activity-spinner workspace-connection-spinner" aria-hidden="true" />
          ) : (
            <BackendConnectionIcon state={descriptor.icon} />
          )}
          <span className="visually-hidden">{descriptor.label}</span>
        </button>
      ) : (
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
      )}
      {hasTooltip ? (
        <div
          id={tooltipId}
          className="activity-tooltip workspace-connection-tooltip"
          role="tooltip"
          aria-hidden={isTooltipVisible ? undefined : true}
        >
          <div className="activity-tooltip-label">{descriptor.label}</div>
          <p>{tooltipDetail}</p>
          {canRetry ? (
            <p className="workspace-connection-tooltip-action">
              Click the status to retry now.
            </p>
          ) : null}
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
