import { describe, expect, it } from "vitest";

import type {
  OrchestratorSessionTemplate,
  OrchestratorTemplateTransition,
} from "../types";
import {
  anchorNormal,
  anchorPosition,
  buildSelfLoopTransitionGeometry,
  buildTransitionGeometry,
  cubicBezierDerivative,
  cubicBezierPoint,
  isValidAnchor,
  nearestAnchorPosition,
  nearestAnchorSide,
  perpendicularOffsetPoint,
} from "./OrchestratorTemplatesPanel";

function makeSession(
  overrides: Partial<OrchestratorSessionTemplate> = {},
): OrchestratorSessionTemplate {
  return {
    id: "builder",
    name: "Builder",
    agent: "Codex",
    model: null,
    instructions: "Implement the change.",
    autoApprove: true,
    inputMode: "queue",
    position: { x: 100, y: 200 },
    ...overrides,
  };
}

function makeTransition(
  overrides: Partial<OrchestratorTemplateTransition> = {},
): OrchestratorTemplateTransition {
  return {
    id: "transition-1",
    fromSessionId: "builder",
    toSessionId: "reviewer",
    trigger: "onCompletion",
    resultMode: "lastResponse",
    promptTemplate: "Continue with:\n{{result}}",
    ...overrides,
  };
}

describe("orchestrator geometry helpers", () => {
  it("accepts valid anchors and rejects invalid values", () => {
    expect(isValidAnchor("left")).toBe(true);
    expect(isValidAnchor("top-right")).toBe(true);
    expect(isValidAnchor(null)).toBe(false);
    expect(isValidAnchor("center" as never)).toBe(false);
  });

  it("returns anchor positions and nearest anchors for a session card", () => {
    const session = makeSession();

    expect(anchorPosition(session, "left")).toEqual({ x: 100, y: 298 });
    expect(anchorPosition(session, "top-right")).toEqual({ x: 420, y: 200 });
    expect(nearestAnchorSide(session, 430, 300)).toBe("right");
    expect(nearestAnchorPosition(session, 250, 205)).toEqual({ x: 260, y: 200 });
  });

  it("returns the expected normal vector for diagonal anchors", () => {
    expect(anchorNormal("bottom-right")).toEqual({
      x: Math.SQRT1_2,
      y: Math.SQRT1_2,
    });
  });

  it("evaluates cubic bezier points and derivatives deterministically", () => {
    const start = { x: 0, y: 0 };
    const control1 = { x: 10, y: 0 };
    const control2 = { x: 10, y: 10 };
    const end = { x: 20, y: 10 };

    expect(cubicBezierPoint(start, control1, control2, end, 0.5)).toEqual({
      x: 10,
      y: 5,
    });
    expect(cubicBezierDerivative(start, control1, control2, end, 0.5)).toEqual({
      x: 15,
      y: 15,
    });
  });

  it("computes perpendicular note offsets even for degenerate vectors", () => {
    expect(perpendicularOffsetPoint(10, 20, 0, 2)).toEqual({ x: -8, y: 20 });
    expect(perpendicularOffsetPoint(10, 20, 0, 0)).toEqual({ x: 10, y: 20 });
  });

  it("builds straight transition geometry between different sessions", () => {
    const fromSession = makeSession({ id: "builder", name: "Builder" });
    const toSession = makeSession({
      id: "reviewer",
      name: "Reviewer",
      position: { x: 600, y: 200 },
    });
    const geometry = buildTransitionGeometry(
      makeTransition(),
      fromSession,
      toSession,
    );

    expect(geometry.path).toBe("M 420 298 L 600 298");
    expect(geometry.startX).toBe(420);
    expect(geometry.startY).toBe(298);
    expect(geometry.endX).toBe(600);
    expect(geometry.endY).toBe(298);
    expect(geometry.midpointX).toBe(510);
    expect(geometry.midpointY).toBe(298);
    expect(geometry.noteX).toBe(510);
    expect(geometry.noteY).toBe(316);
    expect(geometry.title).toBe("transition-1: Builder -> Reviewer");
  });

  it("builds deterministic self-loop geometry with default anchors", () => {
    const session = makeSession();
    const geometry = buildSelfLoopTransitionGeometry(
      makeTransition({
        fromSessionId: session.id,
        toSessionId: session.id,
      }),
      session,
    );

    expect(geometry.path).toBe("M 420 298 C 540 298, 260 80, 260 200");
    expect(geometry.startX).toBe(420);
    expect(geometry.startY).toBe(298);
    expect(geometry.endX).toBe(260);
    expect(geometry.endY).toBe(200);
    expect(geometry.midpointX).toBeCloseTo(385, 6);
    expect(geometry.midpointY).toBeCloseTo(204, 6);
    expect(geometry.noteX).toBeCloseTo(395.5, 3);
    expect(geometry.noteY).toBeCloseTo(189.38, 3);
    expect(geometry.title).toBe("transition-1: Builder -> Builder");
  });

  it("routes self-loop transitions through the self-loop geometry builder", () => {
    const session = makeSession();
    const transition = makeTransition({
      fromSessionId: session.id,
      toSessionId: session.id,
      fromAnchor: "right",
      toAnchor: "right",
    });

    expect(buildTransitionGeometry(transition, session, session)).toEqual(
      buildSelfLoopTransitionGeometry(transition, session),
    );
  });
});