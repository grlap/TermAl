// Type guards and schema shape-types that validate an
// OrchestratorTemplatesPanel state payload restored from
// `localStorage`. Every JSON field we accept is checked here; anything
// that doesn't match the schema causes the guard to return `false` and
// the caller (`readState` in `./OrchestratorTemplatesPanel.tsx`) falls
// back to a fresh empty draft.
//
// What this file owns:
//   - `PersistedOrchestratorSessionTemplate` — alias over the live
//     `OrchestratorSessionTemplate` shape; every validated field is
//     checked below, so this alias exists purely to mark the guard
//     intent (and to make future shape divergence obvious).
//   - `PersistedOrchestratorTemplateTransition` — the on-disk
//     transition shape: the live shape minus the anchor fields,
//     plus optional nullable anchor fields so older payloads
//     without anchors round-trip cleanly.
//   - `SUPPORTED_PERSISTED_TEMPLATE_AGENTS` — the `{ Claude: true,
//     Codex: true, Cursor: true, Gemini: true }` allowlist marked
//     `satisfies Record<AgentType, true>` so new agents break the
//     build if they aren't wired through here.
//   - `objectHasOwnWithFallback` — the `Object.hasOwn` /
//     `Object.prototype.hasOwnProperty.call` shim used when
//     checking the agent allowlist. Exported because
//     `OrchestratorTemplatesPanel.test.tsx` exercises it directly.
//   - `isSupportedPersistedTemplateAgent` — narrows `unknown` to
//     `AgentType` using the allowlist.
//   - `isPersistedSessionTemplate` — full `unknown` → session
//     template guard (id / name / agent / model / instructions /
//     autoApprove / inputMode / finite position.x,y).
//   - `isTransitionTemplate` — full `unknown` → transition
//     template guard (id / from / to / prompt / optional anchors
//     via `isValidAnchor` / trigger / resultMode union).
//
// What this file does NOT own:
//   - The `PanelState` / `InitialPanelState` / `PendingPanelPersistence`
//     state-shape types — those stay with the panel because they
//     describe its React / localStorage state machine.
//   - `readState` / `finalizePanelState` / `resolveInitialState` /
//     `savedDraftForTemplateId` — the functions that *use* these
//     guards — stay with the panel since they pull in `emptyDraft`
//     / `templateToDraft` / `clampPosition` and interact with the
//     browser `localStorage` global.
//   - The `isValidAnchor` anchor-side guard — lives in
//     `./orchestrator-board-geometry.ts` and is imported here.
//
// Split out of `ui/src/panels/OrchestratorTemplatesPanel.tsx`. Same
// checks, same field ordering, same agent allowlist; consumers (the
// panel + existing `OrchestratorTemplatesPanel.test.tsx`) import
// directly from here.

import type {
  AgentType,
  OrchestratorSessionTemplate,
  OrchestratorTemplateTransition,
  OrchestratorTransitionAnchor,
} from "../types";
import { isValidAnchor } from "./orchestrator-board-geometry";

// Keep isPersistedSessionTemplate in sync with every validated
// OrchestratorSessionTemplate field restored from localStorage.
export type PersistedOrchestratorSessionTemplate = OrchestratorSessionTemplate;
export type PersistedOrchestratorTemplateTransition = Omit<
  OrchestratorTemplateTransition,
  "fromAnchor" | "toAnchor"
> & {
  fromAnchor?: OrchestratorTransitionAnchor | null;
  toAnchor?: OrchestratorTransitionAnchor | null;
};

export const SUPPORTED_PERSISTED_TEMPLATE_AGENTS = {
  Claude: true,
  Codex: true,
  Cursor: true,
  Gemini: true,
} satisfies Record<AgentType, true>;

export function objectHasOwnWithFallback(target: object, key: PropertyKey) {
  const objectWithHasOwn = Object as ObjectConstructor & {
    hasOwn?: (target: object, key: PropertyKey) => boolean;
  };
  return (
    objectWithHasOwn.hasOwn?.(target, key) ??
    Object.prototype.hasOwnProperty.call(target, key)
  );
}

export function isSupportedPersistedTemplateAgent(value: unknown): value is AgentType {
  return (
    typeof value === "string" &&
    objectHasOwnWithFallback(SUPPORTED_PERSISTED_TEMPLATE_AGENTS, value)
  );
}

export function isPersistedSessionTemplate(
  value: unknown,
): value is PersistedOrchestratorSessionTemplate {
  if (!value || typeof value !== "object") {
    return false;
  }
  const candidate = value as Partial<PersistedOrchestratorSessionTemplate>;
  return (
    typeof candidate.id === "string" &&
    typeof candidate.name === "string" &&
    isSupportedPersistedTemplateAgent(candidate.agent) &&
    (candidate.model === undefined ||
      candidate.model === null ||
      typeof candidate.model === "string") &&
    typeof candidate.instructions === "string" &&
    typeof candidate.autoApprove === "boolean" &&
    (candidate.inputMode === "queue" ||
      candidate.inputMode === "consolidate") &&
    candidate.position !== null &&
    candidate.position !== undefined &&
    typeof candidate.position === "object" &&
    !Array.isArray(candidate.position) &&
    Number.isFinite(candidate.position.x) &&
    Number.isFinite(candidate.position.y)
  );
}

export function isTransitionTemplate(
  value: unknown,
): value is PersistedOrchestratorTemplateTransition {
  if (!value || typeof value !== "object") {
    return false;
  }
  const candidate = value as Partial<PersistedOrchestratorTemplateTransition>;
  return (
    typeof candidate.id === "string" &&
    typeof candidate.fromSessionId === "string" &&
    typeof candidate.toSessionId === "string" &&
    (candidate.promptTemplate === undefined ||
      candidate.promptTemplate === null ||
      typeof candidate.promptTemplate === "string") &&
    (candidate.fromAnchor === undefined ||
      candidate.fromAnchor === null ||
      isValidAnchor(candidate.fromAnchor)) &&
    (candidate.toAnchor === undefined ||
      candidate.toAnchor === null ||
      isValidAnchor(candidate.toAnchor)) &&
    candidate.trigger === "onCompletion" &&
    (candidate.resultMode === "none" ||
      candidate.resultMode === "lastResponse" ||
      candidate.resultMode === "summary" ||
      candidate.resultMode === "summaryAndLastResponse")
  );
}
