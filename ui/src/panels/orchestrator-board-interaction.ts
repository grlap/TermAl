// Pan + zoom interaction constants and helper types for the
// orchestrator board. Shared by the panel so the mouse-wheel zoom,
// pointer-drag pan, and right-click-vs-pan disambiguation all
// agree on the same thresholds and the same state-shape for their
// refs.
//
// What this file owns:
//   - Zoom range: `MIN_ZOOM = 0.5`, `MAX_ZOOM = 2`, `DEFAULT_ZOOM
//     = 1`. The board zoom factor is clamped into [MIN_ZOOM,
//     MAX_ZOOM] and rounded to 3 decimal places via `clampZoom`.
//   - `WHEEL_ZOOM_SENSITIVITY = 0.002` — the exponential
//     sensitivity applied to `wheel` event `deltaY` when
//     converting to a zoom delta.
//   - `PAN_CONTEXT_MENU_SUPPRESS_THRESHOLD_PX = 4` — the
//     pixel-distance threshold that tells the board's
//     `contextmenu` handler "this was a pan drag, not a right
//     click; don't open the context menu."
//   - `clampZoom` — rounds + clamps a raw zoom factor.
//   - `ZoomAnchor` — the mouse-pinned zoom state captured at the
//     start of a wheel-zoom gesture (canvas coords + scroll
//     offsets) so the board can keep the hovered point
//     stationary on screen while the zoom factor changes.
//   - `PanDragState` — the active pointer-drag state used while
//     middle-click / space+drag panning the board's scroll
//     container.
//
// What this file does NOT own:
//   - Board / card dimensions — live in
//     `./orchestrator-board-geometry.ts`.
//   - Any of the `useState` / `useRef` / event-handler wiring —
//     stays with `<OrchestratorTemplatesPanel>`.
//
// Split out of `ui/src/panels/OrchestratorTemplatesPanel.tsx`.
// Same constants, same rounding, same type shapes.

export const MIN_ZOOM = 0.5;
export const MAX_ZOOM = 2;
export const DEFAULT_ZOOM = 1;
export const WHEEL_ZOOM_SENSITIVITY = 0.002;
export const PAN_CONTEXT_MENU_SUPPRESS_THRESHOLD_PX = 4;

export function clampZoom(value: number): number {
  return (
    Math.round(Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, value)) * 1000) / 1000
  );
}

export type ZoomAnchor = {
  canvasX: number;
  canvasY: number;
  clientOffsetX: number;
  clientOffsetY: number;
  scrollContainer: HTMLElement;
  scrollLeft: number;
  scrollTop: number;
};

export type PanDragState = {
  hasMoved: boolean;
  originScrollLeft: number;
  originScrollTop: number;
  pointerId: number;
  scrollContainer: HTMLElement;
  startClientX: number;
  startClientY: number;
};
