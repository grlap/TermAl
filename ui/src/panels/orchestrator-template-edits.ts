// Pure, React-free helpers that mutate an orchestrator-template
// draft: create new sessions / transitions, clamp node positions
// inside the board frame, pick the next sequence number for a
// given id prefix, and validate a draft before save.
//
// What this file owns:
//   - Capacity constants: `MAX_ORCHESTRATOR_TEMPLATE_SESSIONS`
//     (50) and `MAX_ORCHESTRATOR_TEMPLATE_TRANSITIONS` (200).
//   - Limit-error formatters:
//     `getOrchestratorTemplateSessionLimitError` and
//     `getOrchestratorTemplateTransitionLimitError`. Used both
//     here (by `validateDraft`) and by the panel's UI to decide
//     when to disable the "add session" / "add transition"
//     buttons and what tooltip to show.
//   - `createSession` — builds a new `OrchestratorSessionTemplate`
//     with a unique `session-N` id, a fresh name, the default
//     agent / model / approval / input-mode values, and a grid-
//     flowed position clamped inside the board frame.
//   - `createTransition` — builds a new
//     `OrchestratorTemplateTransition` that connects the first
//     two existing sessions (or the first session to itself when
//     only one exists) using the default `onCompletion` /
//     `lastResponse` / prompt-template triple. Anchors are
//     intentionally left unset so the board picks them from the
//     nearest-anchor math.
//   - `createTransitionBetween` — the explicit-endpoints variant
//     used when the author drags a transition between two nodes;
//     takes `fromAnchor` + `toAnchor` and otherwise matches
//     `createTransition`.
//   - `nextSequenceNumber` — scans a list of ids for
//     `${prefix}${N}` and returns the first unused `N` starting
//     from 1.
//   - `validateDraft` — returns an error string (or `null` when
//     the draft is save-ready) covering: empty template name,
//     empty session list, capacity overruns, duplicate or empty
//     session / transition ids, non-finite canvas positions, and
//     transitions that reference unknown source / destination
//     sessions.
//   - `clampPosition` — rounds + clamps an `(x, y)` to sit inside
//     `[BOARD_MARGIN, BOARD_WIDTH - CARD_WIDTH - BOARD_MARGIN]` ×
//     `[BOARD_MARGIN, BOARD_HEIGHT - CARD_HEIGHT - BOARD_MARGIN]`.
//
// What this file does NOT own:
//   - Board geometry / anchor math — lives in
//     `./orchestrator-board-geometry.ts` and is imported here.
//   - Panel state, persistence, keyboard / pointer wiring, or any
//     of the React components — all of that stays in
//     `./OrchestratorTemplatesPanel.tsx`.
//   - Template schema / persistence (version upgrades,
//     localStorage keys, JSON shape migrations) — those stay with
//     the panel too.
//
// Split out of `ui/src/panels/OrchestratorTemplatesPanel.tsx`.
// Same id prefixes (`session-` / `transition-`), same default
// prompt template text (`Continue with:\n{{result}}`), same
// validation error strings, same clamp rounding (half-to-even
// via `Math.round`).

import type {
  OrchestratorNodePosition,
  OrchestratorSessionTemplate,
  OrchestratorTemplateDraft,
  OrchestratorTemplateTransition,
} from "../types";
import {
  BOARD_HEIGHT,
  BOARD_MARGIN,
  BOARD_WIDTH,
  CARD_HEIGHT,
  CARD_WIDTH,
  type AnchorSide,
} from "./orchestrator-board-geometry";

export const MAX_ORCHESTRATOR_TEMPLATE_SESSIONS = 50;
export const MAX_ORCHESTRATOR_TEMPLATE_TRANSITIONS = 200;

export function getOrchestratorTemplateSessionLimitError() {
  return `Orchestrator templates support at most ${MAX_ORCHESTRATOR_TEMPLATE_SESSIONS} sessions.`;
}

export function getOrchestratorTemplateTransitionLimitError() {
  return `Orchestrator templates support at most ${MAX_ORCHESTRATOR_TEMPLATE_TRANSITIONS} transitions.`;
}

export function createSession(
  existingSessions: OrchestratorSessionTemplate[],
): OrchestratorSessionTemplate {
  const nextNumber = nextSequenceNumber(
    existingSessions.map((session) => session.id),
    "session-",
  );
  const nextX = BOARD_MARGIN + (existingSessions.length % 4) * 380;
  const nextY = 140 + Math.floor(existingSessions.length / 4) * 250;

  return {
    id: `session-${nextNumber}`,
    name: `Session ${nextNumber}`,
    agent: "Codex",
    model: "",
    instructions: "",
    autoApprove: false,
    inputMode: "queue",
    position: clampPosition(nextX, nextY),
  };
}

export function createTransition(
  sessions: OrchestratorSessionTemplate[],
  existingTransitions: OrchestratorTemplateTransition[],
): OrchestratorTemplateTransition {
  const nextNumber = nextSequenceNumber(
    existingTransitions.map((transition) => transition.id),
    "transition-",
  );
  const fromSession = sessions[0];
  const toSession = sessions[1] ?? sessions[0];

  return {
    id: `transition-${nextNumber}`,
    fromSessionId: fromSession?.id ?? "",
    toSessionId: toSession?.id ?? "",
    trigger: "onCompletion",
    resultMode: "lastResponse",
    promptTemplate: "Continue with:\n{{result}}",
  };
}

export function createTransitionBetween(
  fromSessionId: string,
  toSessionId: string,
  fromAnchor: AnchorSide,
  toAnchor: AnchorSide,
  existingTransitions: OrchestratorTemplateTransition[],
): OrchestratorTemplateTransition {
  const nextNumber = nextSequenceNumber(
    existingTransitions.map((transition) => transition.id),
    "transition-",
  );
  return {
    id: `transition-${nextNumber}`,
    fromSessionId,
    toSessionId,
    fromAnchor,
    toAnchor,
    trigger: "onCompletion",
    resultMode: "lastResponse",
    promptTemplate: "Continue with:\n{{result}}",
  };
}

export function nextSequenceNumber(values: string[], prefix: string) {
  let next = 1;
  const seen = new Set(values);
  while (seen.has(`${prefix}${next}`)) {
    next += 1;
  }
  return next;
}

export function validateDraft(draft: OrchestratorTemplateDraft) {
  if (!draft.name.trim()) {
    return "Template name cannot be empty.";
  }

  if (draft.sessions.length === 0) {
    return "Add at least one session before saving.";
  }

  if (draft.sessions.length > MAX_ORCHESTRATOR_TEMPLATE_SESSIONS) {
    return getOrchestratorTemplateSessionLimitError();
  }

  if (draft.transitions.length > MAX_ORCHESTRATOR_TEMPLATE_TRANSITIONS) {
    return getOrchestratorTemplateTransitionLimitError();
  }

  const sessionIds = new Set<string>();
  for (const session of draft.sessions) {
    if (!session.id.trim()) {
      return "Session id cannot be empty.";
    }
    if (!session.name.trim()) {
      return "Session name cannot be empty.";
    }
    if (sessionIds.has(session.id.trim())) {
      return `Duplicate session id \`${session.id.trim()}\`.`;
    }
    sessionIds.add(session.id.trim());
    if (
      !Number.isFinite(session.position.x) ||
      !Number.isFinite(session.position.y)
    ) {
      return `Session \`${session.id.trim()}\` has an invalid canvas position.`;
    }
  }

  const transitionIds = new Set<string>();
  for (const transition of draft.transitions) {
    if (!transition.id.trim()) {
      return "Transition id cannot be empty.";
    }
    if (transitionIds.has(transition.id.trim())) {
      return `Duplicate transition id \`${transition.id.trim()}\`.`;
    }
    transitionIds.add(transition.id.trim());
    if (!sessionIds.has(transition.fromSessionId)) {
      return `Transition \`${transition.id.trim()}\` references an unknown source session.`;
    }
    if (!sessionIds.has(transition.toSessionId)) {
      return `Transition \`${transition.id.trim()}\` references an unknown destination session.`;
    }
  }

  return null;
}

export function clampPosition(x: number, y: number): OrchestratorNodePosition {
  return {
    x: Math.max(
      BOARD_MARGIN,
      Math.min(BOARD_WIDTH - CARD_WIDTH - BOARD_MARGIN, Math.round(x)),
    ),
    y: Math.max(
      BOARD_MARGIN,
      Math.min(BOARD_HEIGHT - CARD_HEIGHT - BOARD_MARGIN, Math.round(y)),
    ),
  };
}
