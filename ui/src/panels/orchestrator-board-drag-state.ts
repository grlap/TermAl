// Pointer-drag state shapes used by the orchestrator board. Both
// types describe active gestures ("I'm dragging a node" or "I'm
// dragging a transition endpoint") and are held in refs / state
// on `<OrchestratorTemplatesPanel>`; exported here so the panel
// can reason about them without inlining the type definitions
// next to its `useState` / `useRef` calls.
//
// What this file owns:
//   - `DragState` — an in-progress node drag: the node id, the
//     owning pointer id, the (x, y) the drag started at, the
//     current (deltaX, deltaY) from that origin, and the client-
//     space start coordinates used to compute the delta on each
//     `pointermove`.
//   - `ConnectionDragState` — an in-progress transition-endpoint
//     drag. Holds the anchor session + side the pointer left
//     from, the owning pointer id, the current cursor position
//     (in canvas coords, for drawing the rubber-band line), and
//     an optional `reconnect` payload that tracks which end of
//     an existing transition is being moved (so the other end
//     stays fixed).
//
// What this file does NOT own:
//   - The `useState` / `useRef` declarations that hold these
//     shapes — stays with the panel.
//   - Anchor-side geometry — lives in
//     `./orchestrator-board-geometry.ts` and is imported here
//     only for the `AnchorSide` type.
//
// Split out of `ui/src/panels/OrchestratorTemplatesPanel.tsx`.
// Same field order, same reconnect nested-shape.

import type { AnchorSide } from "./orchestrator-board-geometry";

export type DragState = {
  nodeId: string;
  pointerId: number;
  originX: number;
  originY: number;
  deltaX: number;
  deltaY: number;
  startClientX: number;
  startClientY: number;
};

export type ConnectionDragState = {
  fromSessionId: string;
  anchorSide: AnchorSide;
  pointerId: number;
  cursorX: number;
  cursorY: number;
  /** When reconnecting an existing transition, tracks which end is fixed. */
  reconnect?: {
    transitionId: string;
    movingEnd: "from" | "to";
    fixedSessionId: string;
    fixedAnchor: AnchorSide;
  };
};
