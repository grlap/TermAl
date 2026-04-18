// Control-panel layout helpers.
//
// What this file owns:
//   - `getDockedControlPanelWidthRatioForWorkspace` — reads the
//     width ratio of the control-panel pane when the root of the
//     workspace is a horizontal split with the control panel on
//     one side. Returns `null` when the tree isn't shaped that way.
//   - `resolvePreferredControlPanelWidthRatio` — picks the best
//     width ratio for the control panel: the current docked ratio
//     if present, otherwise the viewport-derived minimum.
//   - `hydrateControlPanelLayout` — makes sure a workspace has a
//     control panel pane and that it sits at the requested edge
//     with the preferred width.
//   - `resolveStandaloneControlPanelDockWidthRatio` — converts a
//     fallback ratio into a concrete ratio, respecting the CSS
//     variables that control the minimum usable dock width so the
//     control panel can't collapse below its rendered minimum.
//   - `resolveRootCssLengthPx` — reads a CSS length variable from
//     `<html>` and returns its computed pixel value, with `rem`
//     and simple `calc(n * length)` support. Internal helper but
//     exported so unit tests / future layout code can reuse it.
//   - Constants that pair with the CSS: `DEFAULT_SPLIT_MIN_RATIO`,
//     `DEFAULT_SPLIT_MAX_RATIO`,
//     `CONTROL_PANEL_PANE_MIN_WIDTH_FALLBACK_PX`,
//     `CONTROL_PANEL_PANE_WIDTH_FALLBACK_PX`,
//     `STANDALONE_CONTROL_SURFACE_PANE_MIN_WIDTH_FALLBACK_PX`.
//
// What this file does NOT own:
//   - Lower-level workspace state primitives — those live in
//     `./workspace.ts` (`ensureControlPanelInWorkspaceState`,
//     `dockControlPanelAtWorkspaceEdge`, tree operations,
//     `DEFAULT_CONTROL_PANEL_DOCK_WIDTH_RATIO`).
//   - React state or components. All functions are pure; the two
//     that read the DOM (`resolveStandaloneControlPanelDockWidthRatio`,
//     `resolveRootCssLengthPx`) do so without side effects.
//
// Split out of `ui/src/App.tsx`. Same function signatures and
// behaviour as the inline definitions they replaced. Lives outside
// `workspace.ts` because these are higher-level compositions of the
// workspace primitives and bringing them into `workspace.ts` would
// grow that already-large module further and mix layout heuristics
// (CSS readout, viewport math) into the pure tree ops.

import { clamp } from "./app-utils";
import {
  DEFAULT_CONTROL_PANEL_DOCK_WIDTH_RATIO,
  dockControlPanelAtWorkspaceEdge,
  ensureControlPanelInWorkspaceState,
  type WorkspaceState,
} from "./workspace";
import type { ControlPanelSide } from "./workspace-storage";

export const DEFAULT_SPLIT_MIN_RATIO = 0.22;
export const DEFAULT_SPLIT_MAX_RATIO = 0.78;
// 40rem is the minimum acceptable docked control-panel width. Keep
// these fallbacks aligned with the CSS dock width/min-width so saved
// layouts do not permit a narrower manual resize that later snaps
// back.
export const CONTROL_PANEL_PANE_MIN_WIDTH_FALLBACK_PX = 40 * 16;
export const STANDALONE_CONTROL_SURFACE_PANE_MIN_WIDTH_FALLBACK_PX = 16 * 16;
export const CONTROL_PANEL_PANE_WIDTH_FALLBACK_PX = 40 * 16;

export function getDockedControlPanelWidthRatioForWorkspace(
  workspace: WorkspaceState,
): number | null {
  const controlPanelPaneId =
    workspace.panes.find((pane) =>
      pane.tabs.some((tab) => tab.kind === "controlPanel"),
    )?.id ?? null;
  if (
    !controlPanelPaneId ||
    !workspace.root ||
    workspace.root.type !== "split" ||
    workspace.root.direction !== "row"
  ) {
    return null;
  }

  if (
    workspace.root.first.type === "pane" &&
    workspace.root.first.paneId === controlPanelPaneId
  ) {
    return workspace.root.ratio;
  }

  if (
    workspace.root.second.type === "pane" &&
    workspace.root.second.paneId === controlPanelPaneId
  ) {
    return 1 - workspace.root.ratio;
  }

  return null;
}

export function resolvePreferredControlPanelWidthRatio(
  workspace: WorkspaceState,
): number {
  const minimumWidthRatio = resolveStandaloneControlPanelDockWidthRatio(
    DEFAULT_CONTROL_PANEL_DOCK_WIDTH_RATIO,
  );
  const currentWidthRatio =
    getDockedControlPanelWidthRatioForWorkspace(workspace);

  return currentWidthRatio === null
    ? minimumWidthRatio
    : Math.max(currentWidthRatio, minimumWidthRatio);
}

export function hydrateControlPanelLayout(
  workspace: WorkspaceState,
  side: ControlPanelSide,
): WorkspaceState {
  const workspaceWithControlPanel =
    ensureControlPanelInWorkspaceState(workspace);

  return dockControlPanelAtWorkspaceEdge(
    workspaceWithControlPanel,
    side,
    resolvePreferredControlPanelWidthRatio(workspaceWithControlPanel),
  );
}

export function resolveStandaloneControlPanelDockWidthRatio(
  fallbackRatio: number,
): number {
  if (typeof document === "undefined") {
    return fallbackRatio;
  }

  const workspaceStage =
    document.querySelector(
      ".workspace-stage.workspace-stage-control-panel-only",
    ) ?? document.querySelector(".workspace-stage");
  const stageWidth =
    workspaceStage instanceof HTMLElement && workspaceStage.clientWidth > 0
      ? workspaceStage.clientWidth
      : (document.documentElement?.clientWidth ??
        (typeof window !== "undefined" ? window.innerWidth : 0));
  if (stageWidth <= 0) {
    return fallbackRatio;
  }

  const controlPanelWidthRatio =
    resolveRootCssLengthPx(
      "--control-panel-pane-width",
      CONTROL_PANEL_PANE_WIDTH_FALLBACK_PX,
    ) / stageWidth;
  const controlPanelMinRatio = clamp(
    resolveRootCssLengthPx(
      "--control-panel-pane-min-width",
      CONTROL_PANEL_PANE_MIN_WIDTH_FALLBACK_PX,
    ) / stageWidth,
    0,
    1,
  );
  const sessionMinRatio = DEFAULT_SPLIT_MIN_RATIO;
  const maxRatio = 1 - sessionMinRatio;

  if (controlPanelMinRatio <= maxRatio) {
    return clamp(controlPanelWidthRatio, controlPanelMinRatio, maxRatio);
  }

  return clamp(
    controlPanelMinRatio /
      Math.max(controlPanelMinRatio + sessionMinRatio, Number.EPSILON),
    0,
    1,
  );
}

export function resolveRootCssLengthPx(
  cssVariableName: string,
  fallbackPx: number,
): number {
  if (typeof window === "undefined" || typeof document === "undefined") {
    return fallbackPx;
  }

  const rootStyle = window.getComputedStyle(document.documentElement);
  const rawValue = rootStyle.getPropertyValue(cssVariableName).trim();
  if (!rawValue) {
    return fallbackPx;
  }

  const rootFontSizePx = Number.parseFloat(rootStyle.fontSize);
  const resolvedValue = rawValue.replace(
    /var\((--[\w-]+)\)/g,
    (_, variableName: string) =>
      rootStyle.getPropertyValue(variableName).trim() || "0",
  );
  const convertLengthToPx = (value: string): number | null => {
    const numericValue = Number.parseFloat(value);
    if (!Number.isFinite(numericValue)) {
      return null;
    }

    if (value.endsWith("rem")) {
      return (
        numericValue * (Number.isFinite(rootFontSizePx) ? rootFontSizePx : 16)
      );
    }

    if (value.endsWith("px") || /^-?\d*\.?\d+$/.test(value)) {
      return numericValue;
    }

    return null;
  };
  const directLengthPx = convertLengthToPx(resolvedValue);
  if (directLengthPx !== null) {
    return directLengthPx;
  }

  const calcMultiplicationMatch = resolvedValue.match(
    /^calc\(\s*([^)]+?)\s*\*\s*([^)]+?)\s*\)$/i,
  );
  if (calcMultiplicationMatch) {
    const left = convertLengthToPx(calcMultiplicationMatch[1].trim());
    const right = Number.parseFloat(calcMultiplicationMatch[2].trim());
    if (left !== null && Number.isFinite(right)) {
      return left * right;
    }

    const rightLengthPx = convertLengthToPx(calcMultiplicationMatch[2].trim());
    const leftScalar = Number.parseFloat(calcMultiplicationMatch[1].trim());
    if (rightLengthPx !== null && Number.isFinite(leftScalar)) {
      return leftScalar * rightLengthPx;
    }
  }

  return fallbackPx;
}
