// Owns live-state pruning helpers for unknown-model send confirmations.
// Does not own model option matching, warning copy, or send-attempt decisions.
// Split from app-live-state.ts to keep adoption cleanup logic testable.
import {
  describeUnknownSessionModelWarning,
  unknownSessionModelConfirmationKey,
} from "./session-model-utils";
import type { Session } from "./types";

export function buildUnknownModelConfirmationKeySet(sessions: Session[]) {
  return new Set(
    sessions
      .filter((session) => describeUnknownSessionModelWarning(session))
      .map((session) =>
        unknownSessionModelConfirmationKey(session.id, session.model),
      ),
  );
}

export function setContainsOnlyValuesFrom<T>(current: Set<T>, allowed: Set<T>) {
  for (const value of current) {
    if (!allowed.has(value)) {
      return false;
    }
  }

  return true;
}
