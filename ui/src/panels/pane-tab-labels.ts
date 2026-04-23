// Pure helpers that format a `WorkspaceTab` into the text shown
// on its tab chip (visible label) vs. the longer accessible label.
//
// What this file owns:
//   - `formatVisibleTabLabel` — the short label shown on the tab
//     itself. For filesystem / git-status / terminal tabs this is
//     just the last path segment (or a fallback), so the tab chip
//     stays narrow. Other kinds fall through to `formatTabLabel`.
//   - `formatTabLabel` — the longer label used as the
//     accessible-name / tooltip for each tab kind. Prefixes like
//     "Files: ", "Git status: ", "Terminal: ", "Instructions: ",
//     "Diff: ", "Orchestration: " disambiguate when multiple tab
//     kinds reference the same path.
//   - `formatPathTabLabel` — last-segment extraction with a
//     fallback string for empty / null paths. Splits on both
//     forward and back slashes so Windows / POSIX paths produce
//     the same label.
//
// What this file does NOT own:
//   - The tab chip itself or its context menu — those live in
//     `./PaneTabs.tsx`.
//   - Session-level tooltip rows (model / project / etc.) — those
//     live with `SessionTabStatusTooltip` in
//     `./session-tab-status-tooltip.tsx`.
//   - Rate-limit or status-indicator formatting — unrelated
//     concern that lives alongside the rate-limit meter in
//     `./session-tab-status-tooltip.tsx`.
//
// Split out of `ui/src/panels/PaneTabs.tsx`. Same function
// bodies, same prefixes, same fallbacks; consumers import
// directly from here.

import type { SessionSummarySnapshot } from "../session-store";
import type { WorkspaceTab } from "../workspace";

export function formatVisibleTabLabel(
  tab: WorkspaceTab,
  session: SessionSummarySnapshot | null,
) {
  if (tab.kind === "filesystem") {
    return formatPathTabLabel(tab.rootPath, "Workspace");
  }

  if (tab.kind === "gitStatus") {
    return formatPathTabLabel(tab.workdir, "Workspace");
  }

  if (tab.kind === "terminal") {
    return formatPathTabLabel(tab.workdir, "Terminal");
  }

  return formatTabLabel(tab, session);
}

export function formatTabLabel(
  tab: WorkspaceTab,
  session: SessionSummarySnapshot | null,
) {
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
    return `Git status: ${formatPathTabLabel(tab.workdir, "Workspace")}`;
  }

  if (tab.kind === "terminal") {
    return `Terminal: ${formatPathTabLabel(tab.workdir, "Workspace")}`;
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

export function formatPathTabLabel(path: string | null, fallback: string) {
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
