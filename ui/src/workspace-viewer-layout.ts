// Browser-layout policy for automatic source/diff viewer splits.
//
// What this file owns:
//   - The minimum usable width for each half of an automatic viewer split.
//   - Reading the rendered opener-pane width and deciding whether splitting it
//     would leave both the conversation and editor usable.
//
// What this file does NOT own:
//   - Workspace tree mutation or pane-role routing; those stay in workspace.ts.
//   - Explicit user splits and drag/drop placement, which always remain exact.

import { resolveRootCssLengthPx } from "./control-panel-layout";

export const WORKSPACE_VIEWER_PANE_MIN_WIDTH_FALLBACK_PX = 30 * 16;
const WORKSPACE_ROW_DIVIDER_WIDTH_FALLBACK_PX = 16;

export function hasRoomForWorkspaceViewerSplit(
  paneWidthPx: number,
  viewerPaneMinWidthPx: number,
  dividerWidthPx = WORKSPACE_ROW_DIVIDER_WIDTH_FALLBACK_PX,
) {
  return (
    Number.isFinite(paneWidthPx) &&
    paneWidthPx > 0 &&
    paneWidthPx >= viewerPaneMinWidthPx * 2 + dividerWidthPx
  );
}

export function workspacePaneHasRoomForViewerSplit(paneId: string) {
  if (typeof document === "undefined") {
    return false;
  }

  const pane = Array.from(
    document.querySelectorAll<HTMLElement>(
      ".workspace-pane[data-workspace-pane-id]",
    ),
  ).find((candidate) => candidate.dataset.workspacePaneId === paneId);
  if (!pane) {
    // Automatic layout mutation requires positive geometry evidence. A stale
    // workspace snapshot or an element between React commits must never be
    // interpreted as unlimited room, because that creates an unreadably thin
    // viewer pane in an already-split workspace.
    return false;
  }

  const paneWidthPx = pane.getBoundingClientRect().width || pane.clientWidth;
  const viewerPaneMinWidthPx = Math.max(
    WORKSPACE_VIEWER_PANE_MIN_WIDTH_FALLBACK_PX,
    resolveRootCssLengthPx(
      "--workspace-viewer-pane-min-width",
      WORKSPACE_VIEWER_PANE_MIN_WIDTH_FALLBACK_PX,
    ),
  );
  return hasRoomForWorkspaceViewerSplit(paneWidthPx, viewerPaneMinWidthPx);
}
