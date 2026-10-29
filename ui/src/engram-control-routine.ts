// Classifies an Engram control card as routine, for the transcript's default
// collapse. Owns only the conservative whitelist: a card is routine when every
// classification field proves an ordinary enforced, turn-gated grant that was
// sent on that grant, at dispatch or checkpoint, with nothing to act on.
// Does not own rendering, the disclosure state or the change marker
// (message-cards.tsx, app-utils.ts). New file, not split from another.
import type { EngramControlMessage } from "./types";

/** The production assurance of a turn-gated control decision. */
const TURN_GATED_ASSURANCE = "turn_gated";

/**
 * True only when the whole conjunction holds. An unknown or unexpected value
 * in any classification field makes the card non-routine, so it stays
 * expanded: success is never inferred from the title or from failure prose.
 */
export function isRoutineEngramControl(message: EngramControlMessage): boolean {
  return (
    message.schemaVersion === 1 &&
    (message.stage === "dispatch" || message.stage === "checkpoint") &&
    message.assurance === TURN_GATED_ASSURANCE &&
    message.decision === "grant" &&
    message.dispatch === "sent_on_grant" &&
    message.failMode === "enforced" &&
    message.causalFailure == null &&
    (message.refusalCode == null || message.refusalCode === "") &&
    (message.deferCode == null || message.deferCode === "") &&
    (message.repairArmed == null || message.repairArmed === false) &&
    (message.directives == null || message.directives.length === 0) &&
    (message.nextIntent == null ||
      message.nextIntent === "continue" ||
      message.nextIntent === "exit")
  );
}
