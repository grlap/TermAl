// Pure geometry helpers for the orchestrator-template board:
// anchor-side math, cubic Bezier sampling, transition-edge path
// construction, and self-loop curve math.
//
// What this file owns:
//   - Board dimensions + pan/zoom padding: `BOARD_WIDTH`,
//     `BOARD_HEIGHT`, `BOARD_MARGIN`. The board frame, pan / zoom
//     math, and node-position clamping all share these numbers.
//   - Per-card dimensions: `CARD_WIDTH` / `CARD_HEIGHT`. The
//     board / panel layout also references these — they live
//     here because anchor-position math and transition geometry
//     read them on every frame.
//   - Self-loop tuning constants: `SELF_LOOP_CONTROL_DISTANCE`,
//     `SELF_LOOP_CONTROL_SPREAD` (how far apart the two Bezier
//     control points sit when the start / end anchors land on
//     the same coordinate).
//   - `TRANSITION_NOTE_OFFSET` — perpendicular distance the
//     transition's note chip sits off the edge's midpoint
//     tangent.
//   - `AnchorSide` type + `ANCHOR_SIDES` readonly array of the
//     eight anchor positions (top, top-right, right,
//     bottom-right, bottom, bottom-left, left, top-left).
//   - `TransitionGeometry` — the per-transition render payload
//     the board consumes (path string + endpoints + note
//     position + accessible title).
//   - `isValidAnchor` — type guard over `AnchorSide`.
//   - `buildTransitionGeometry` — straight-line transition
//     between two distinct session nodes; delegates to
//     `buildSelfLoopTransitionGeometry` when the two endpoints
//     are the same node.
//   - `buildSelfLoopTransitionGeometry` — cubic Bezier self-loop
//     with smart control-point placement: if the author picked
//     identical start/end anchors, the math expands
//     perpendicular to the node normal using
//     `SELF_LOOP_CONTROL_SPREAD`; otherwise each control point
//     extends along its own anchor's normal.
//   - `anchorNormal` — unit vector pointing away from the card
//     at the given anchor side (diagonals use
//     `Math.SQRT1_2`).
//   - `anchorPosition` — absolute board coordinate of the
//     anchor on a given session card.
//   - `nearestAnchorSide`, `nearestAnchorPosition` — pick the
//     anchor closest to a cursor point during connection-drag.
//   - `cubicBezierPoint`, `cubicBezierDerivative` — de Casteljau
//     sampler and derivative (for midpoint + tangent).
//   - `perpendicularOffsetPoint` — offset a point perpendicular
//     to a direction vector, used to place the transition note.
//   - `defaultSelfLoopEndAnchor` — when the author only picked
//     one endpoint anchor for a self-loop, pick a sensible
//     perpendicular one for the other end.
//
// What this file does NOT own:
//   - Anything React / DOM. All helpers are pure.
//   - Panel state, board panning / zooming, template
//     persistence, or the Monaco-based template editors — those
//     live in `./OrchestratorTemplatesPanel.tsx`.
//   - The shared orchestrator template / transition / node
//     types — those live in `../types` and are imported here.
//
// Split out of `ui/src/panels/OrchestratorTemplatesPanel.tsx`.
// Same function bodies, same constants, same coordinate
// conventions; consumers (the panel itself + the existing
// `OrchestratorTemplatesPanel.geometry.test.ts`) import from
// here directly.

import type {
  OrchestratorNodePosition,
  OrchestratorSessionTemplate,
  OrchestratorTemplateTransition,
  OrchestratorTransitionAnchor,
} from "../types";

export const BOARD_WIDTH = 2560;
export const BOARD_HEIGHT = 1600;
export const BOARD_MARGIN = 32;
export const CARD_WIDTH = 320;
export const CARD_HEIGHT = 176;
export const SELF_LOOP_CONTROL_DISTANCE = 120;
export const SELF_LOOP_CONTROL_SPREAD = 56;
export const TRANSITION_NOTE_OFFSET = 18;

export type AnchorSide = OrchestratorTransitionAnchor;

export const ANCHOR_SIDES: readonly AnchorSide[] = [
  "top",
  "top-right",
  "right",
  "bottom-right",
  "bottom",
  "bottom-left",
  "left",
  "top-left",
];

export type TransitionGeometry = {
  transition: OrchestratorTemplateTransition;
  path: string;
  startX: number;
  startY: number;
  endX: number;
  endY: number;
  midpointX: number;
  midpointY: number;
  noteX: number;
  noteY: number;
  title: string;
};

export function isValidAnchor(
  value: OrchestratorTransitionAnchor | null | undefined,
): value is AnchorSide {
  return ANCHOR_SIDES.includes(value as AnchorSide);
}

export function buildTransitionGeometry(
  transition: OrchestratorTemplateTransition,
  fromNode: OrchestratorSessionTemplate,
  toNode: OrchestratorSessionTemplate,
): TransitionGeometry {
  if (fromNode.id === toNode.id) {
    return buildSelfLoopTransitionGeometry(transition, fromNode);
  }

  const toCenter = {
    x: toNode.position.x + CARD_WIDTH / 2,
    y: toNode.position.y + CARD_HEIGHT / 2,
  };
  const fromCenter = {
    x: fromNode.position.x + CARD_WIDTH / 2,
    y: fromNode.position.y + CARD_HEIGHT / 2,
  };
  const start = isValidAnchor(transition.fromAnchor)
    ? anchorPosition(fromNode, transition.fromAnchor)
    : nearestAnchorPosition(fromNode, toCenter.x, toCenter.y);
  const end = isValidAnchor(transition.toAnchor)
    ? anchorPosition(toNode, transition.toAnchor)
    : nearestAnchorPosition(toNode, fromCenter.x, fromCenter.y);
  const midpointX = (start.x + end.x) / 2;
  const midpointY = (start.y + end.y) / 2;
  const dx = end.x - start.x;
  const dy = end.y - start.y;
  const note = perpendicularOffsetPoint(midpointX, midpointY, dx, dy);

  return {
    transition,
    path: `M ${start.x} ${start.y} L ${end.x} ${end.y}`,
    startX: start.x,
    startY: start.y,
    endX: end.x,
    endY: end.y,
    midpointX,
    midpointY,
    noteX: note.x,
    noteY: note.y,
    title: `${transition.id}: ${fromNode.name || fromNode.id} -> ${toNode.name || toNode.id}`,
  };
}

export function buildSelfLoopTransitionGeometry(
  transition: OrchestratorTemplateTransition,
  session: OrchestratorSessionTemplate,
): TransitionGeometry {
  const fromSide = isValidAnchor(transition.fromAnchor)
    ? transition.fromAnchor
    : "right";
  const rawToSide = isValidAnchor(transition.toAnchor)
    ? transition.toAnchor
    : "top";
  const toSide =
    rawToSide === fromSide && !isValidAnchor(transition.toAnchor)
      ? defaultSelfLoopEndAnchor(fromSide)
      : rawToSide;
  const start = anchorPosition(session, fromSide);
  const end = anchorPosition(session, toSide);

  let control1: OrchestratorNodePosition;
  let control2: OrchestratorNodePosition;
  if (start.x === end.x && start.y === end.y) {
    const normal = anchorNormal(fromSide);
    const tangent = { x: -normal.y, y: normal.x };
    control1 = {
      x:
        start.x +
        normal.x * SELF_LOOP_CONTROL_DISTANCE +
        tangent.x * SELF_LOOP_CONTROL_SPREAD,
      y:
        start.y +
        normal.y * SELF_LOOP_CONTROL_DISTANCE +
        tangent.y * SELF_LOOP_CONTROL_SPREAD,
    };
    control2 = {
      x:
        end.x +
        normal.x * SELF_LOOP_CONTROL_DISTANCE -
        tangent.x * SELF_LOOP_CONTROL_SPREAD,
      y:
        end.y +
        normal.y * SELF_LOOP_CONTROL_DISTANCE -
        tangent.y * SELF_LOOP_CONTROL_SPREAD,
    };
  } else {
    const startNormal = anchorNormal(fromSide);
    const endNormal = anchorNormal(toSide);
    control1 = {
      x: start.x + startNormal.x * SELF_LOOP_CONTROL_DISTANCE,
      y: start.y + startNormal.y * SELF_LOOP_CONTROL_DISTANCE,
    };
    control2 = {
      x: end.x + endNormal.x * SELF_LOOP_CONTROL_DISTANCE,
      y: end.y + endNormal.y * SELF_LOOP_CONTROL_DISTANCE,
    };
  }

  const midpoint = cubicBezierPoint(start, control1, control2, end, 0.5);
  const tangent = cubicBezierDerivative(start, control1, control2, end, 0.5);
  const note = perpendicularOffsetPoint(
    midpoint.x,
    midpoint.y,
    tangent.x,
    tangent.y,
  );

  return {
    transition,
    path: `M ${start.x} ${start.y} C ${control1.x} ${control1.y}, ${control2.x} ${control2.y}, ${end.x} ${end.y}`,
    startX: start.x,
    startY: start.y,
    endX: end.x,
    endY: end.y,
    midpointX: midpoint.x,
    midpointY: midpoint.y,
    noteX: note.x,
    noteY: note.y,
    title: `${transition.id}: ${session.name || session.id} -> ${session.name || session.id}`,
  };
}

function defaultSelfLoopEndAnchor(side: AnchorSide): AnchorSide {
  switch (side) {
    case "top":
      return "right";
    case "top-right":
      return "right";
    case "right":
      return "top";
    case "bottom-right":
      return "right";
    case "bottom":
      return "right";
    case "bottom-left":
      return "bottom";
    case "left":
      return "top";
    case "top-left":
      return "top";
  }
}

export function anchorNormal(side: AnchorSide): OrchestratorNodePosition {
  switch (side) {
    case "top":
      return { x: 0, y: -1 };
    case "top-right":
      return { x: Math.SQRT1_2, y: -Math.SQRT1_2 };
    case "right":
      return { x: 1, y: 0 };
    case "bottom-right":
      return { x: Math.SQRT1_2, y: Math.SQRT1_2 };
    case "bottom":
      return { x: 0, y: 1 };
    case "bottom-left":
      return { x: -Math.SQRT1_2, y: Math.SQRT1_2 };
    case "left":
      return { x: -1, y: 0 };
    case "top-left":
      return { x: -Math.SQRT1_2, y: -Math.SQRT1_2 };
  }
}

export function cubicBezierPoint(
  start: OrchestratorNodePosition,
  control1: OrchestratorNodePosition,
  control2: OrchestratorNodePosition,
  end: OrchestratorNodePosition,
  t: number,
): OrchestratorNodePosition {
  const inverse = 1 - t;
  return {
    x:
      inverse ** 3 * start.x +
      3 * inverse ** 2 * t * control1.x +
      3 * inverse * t ** 2 * control2.x +
      t ** 3 * end.x,
    y:
      inverse ** 3 * start.y +
      3 * inverse ** 2 * t * control1.y +
      3 * inverse * t ** 2 * control2.y +
      t ** 3 * end.y,
  };
}

export function cubicBezierDerivative(
  start: OrchestratorNodePosition,
  control1: OrchestratorNodePosition,
  control2: OrchestratorNodePosition,
  end: OrchestratorNodePosition,
  t: number,
): OrchestratorNodePosition {
  const inverse = 1 - t;
  return {
    x:
      3 * inverse ** 2 * (control1.x - start.x) +
      6 * inverse * t * (control2.x - control1.x) +
      3 * t ** 2 * (end.x - control2.x),
    y:
      3 * inverse ** 2 * (control1.y - start.y) +
      6 * inverse * t * (control2.y - control1.y) +
      3 * t ** 2 * (end.y - control2.y),
  };
}

export function perpendicularOffsetPoint(
  x: number,
  y: number,
  dx: number,
  dy: number,
): OrchestratorNodePosition {
  const length = Math.hypot(dx, dy) || 1;
  return {
    x: x - (dy / length) * TRANSITION_NOTE_OFFSET,
    y: y + (dx / length) * TRANSITION_NOTE_OFFSET,
  };
}

export function anchorPosition(
  session: OrchestratorSessionTemplate,
  side: AnchorSide,
): OrchestratorNodePosition {
  const x = session.position.x;
  const y = session.position.y;
  const cx = x + CARD_WIDTH / 2;
  const cy = y + CARD_HEIGHT / 2;
  switch (side) {
    case "top":
      return { x: cx, y };
    case "top-right":
      return { x: x + CARD_WIDTH, y };
    case "right":
      return { x: x + CARD_WIDTH, y: cy };
    case "bottom-right":
      return { x: x + CARD_WIDTH, y: y + CARD_HEIGHT };
    case "bottom":
      return { x: cx, y: y + CARD_HEIGHT };
    case "bottom-left":
      return { x, y: y + CARD_HEIGHT };
    case "left":
      return { x, y: cy };
    case "top-left":
      return { x, y };
  }
}

export function nearestAnchorSide(
  session: OrchestratorSessionTemplate,
  cursorX: number,
  cursorY: number,
): AnchorSide {
  let bestSide: AnchorSide = "top";
  let bestDist = Infinity;
  for (const side of ANCHOR_SIDES) {
    const anchor = anchorPosition(session, side);
    const dist = Math.hypot(cursorX - anchor.x, cursorY - anchor.y);
    if (dist < bestDist) {
      bestDist = dist;
      bestSide = side;
    }
  }
  return bestSide;
}

export function nearestAnchorPosition(
  session: OrchestratorSessionTemplate,
  cursorX: number,
  cursorY: number,
): OrchestratorNodePosition {
  let bestAnchor: OrchestratorNodePosition | null = null;
  let bestDist = Infinity;
  for (const side of ANCHOR_SIDES) {
    const anchor = anchorPosition(session, side);
    const dist = Math.hypot(cursorX - anchor.x, cursorY - anchor.y);
    if (dist < bestDist) {
      bestDist = dist;
      bestAnchor = anchor;
    }
  }
  return (
    bestAnchor ?? {
      x: session.position.x + CARD_WIDTH / 2,
      y: session.position.y + CARD_HEIGHT / 2,
    }
  );
}
