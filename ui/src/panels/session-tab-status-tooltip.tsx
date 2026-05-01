// The `<SessionTabStatusTooltip>` portal component + the label /
// row builders that drive the tooltip body, plus the Codex rate-
// limit meter sub-component it renders when a Codex session exposes
// rate-limit windows.
//
// What this file owns:
//   - `SessionTabStatusTooltip` — the React component that renders
//     the pane-tab status hover card (Agent / State / Last response /
//     Project / Location / Model / optional Session / Policy / Sandbox /
//     Reasoning / Thread / Approval / Effort / Mode rows, 5h + 7d
//     Codex rate-limit meters, Codex notices list).
//   - `CodexRateLimitMeter` — the tiny inline component that draws
//     the remaining/used bar and the resets-at caption.
//   - Tooltip row types + builders: `SessionTooltipRow`,
//     `buildSessionTooltipRows`, `formatSessionTooltipModelRow`,
//     `formatSessionTooltipProjectLabel`,
//     `formatSessionTooltipLocationLabel`.
//   - Label formatters: `formatCodexNoticeBadgeLabel`,
//     `formatTooltipEnumLabel`, `formatRateLimitResetLabel`.
//   - Visibility predicate: `hasSessionTabStatusTooltip`.
//   - Private `clamp` helper used by `CodexRateLimitMeter` for
//     percentage clamping.
//
// What this file does NOT own:
//   - The tooltip positioning math — that lives in
//     `../pane-tab-status-tooltip.ts` (`measurePaneTabStatusTooltipPosition`,
//     `PaneTabTooltipAnchorRect`, `PaneTabStatusTooltipPosition`).
//   - The `<PaneTabs>` component itself, the tab-bar layout, or the
//     hover / focus state that decides when to mount this tooltip —
//     all of that stays in `./PaneTabs.tsx`.
//   - Remote display primitives (`createBuiltinLocalRemote`,
//     `isLocalRemoteId`, `remoteConnectionLabel`,
//     `remoteDisplayName`, `resolveProjectRemoteId`) — live in
//     `../remotes`.
//   - Session model option resolution — lives in
//     `../session-model-options`.
//
// Split out of `ui/src/panels/PaneTabs.tsx`. Same component tree,
// same class names, same row ordering, same copy.

import { Fragment, type CSSProperties } from "react";
import {
  createBuiltinLocalRemote,
  isLocalRemoteId,
  remoteConnectionLabel,
  remoteDisplayName,
  resolveProjectRemoteId,
} from "../remotes";
import { matchingSessionModelOption } from "../session-model-options";
import type { SessionSummarySnapshot } from "../session-store";
import type {
  CodexRateLimitWindow,
  CodexState,
  Project,
  RemoteConfig,
} from "../types";

export function SessionTabStatusTooltip({
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
  session: SessionSummarySnapshot;
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

export function formatCodexNoticeBadgeLabel(count: number) {
  return `${count} Codex notice${count === 1 ? "" : "s"}`;
}

export function hasSessionTabStatusTooltip(_session: SessionSummarySnapshot) {
  return true;
}

export function formatSessionTooltipProjectLabel(
  session: SessionSummarySnapshot,
  projectLookup: ReadonlyMap<string, Project>,
) {
  const projectId = session.projectId?.trim() ?? "";
  if (!projectId) {
    return "Workspace only";
  }

  return projectLookup.get(projectId)?.name ?? "Missing project";
}

export function formatSessionTooltipLocationLabel(
  session: SessionSummarySnapshot,
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

export type SessionTooltipRow = {
  key: string;
  value: string;
  mono?: boolean;
};

export function formatSessionTooltipModelRow(
  session: SessionSummarySnapshot,
): SessionTooltipRow {
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

export function buildSessionTooltipRows(
  session: SessionSummarySnapshot,
  projectLookup: ReadonlyMap<string, Project>,
  remoteLookup: ReadonlyMap<string, RemoteConfig>,
): SessionTooltipRow[] {
  const rows: SessionTooltipRow[] = [
    { key: "Agent", value: session.agent },
    { key: "State", value: formatTooltipEnumLabel(session.status) },
    ...(session.lastResponseTimestamp
      ? [{ key: "Last response", value: session.lastResponseTimestamp }]
      : []),
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

export function formatTooltipEnumLabel(value: string) {
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

export function CodexRateLimitMeter({
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

export function formatRateLimitResetLabel(resetsAt: number | null, label: string) {
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
