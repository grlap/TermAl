// Owns live-state delta event type guards and session-id extraction helpers.
// Does not own delta application, revision decisions, SSE transport, or retry scheduling.
// Split from app-live-state.ts to keep the hook's main state machine smaller.
import type {
  DeltaEvent,
} from "./types";
import type { SessionDeltaEvent } from "./live-updates";

export type DelegationDeltaEvent = Extract<
  DeltaEvent,
  {
    type:
      | "delegationCreated"
      | "delegationWaitCreated"
      | "delegationWaitConsumed"
      | "delegationWaitResumeDispatchFailed"
      | "delegationUpdated"
      | "delegationCompleted"
      | "delegationFailed"
      | "delegationCanceled";
  }
>;

export function isSessionDeltaEvent(delta: DeltaEvent): delta is SessionDeltaEvent {
  return "sessionId" in delta && typeof delta.sessionId === "string";
}

export function isSameRevisionReplayableSessionDelta(
  delta: DeltaEvent,
): delta is Exclude<SessionDeltaEvent, { type: "textDelta" }> {
  // Same-revision state snapshots may carry only summary data. Idempotent
  // session deltas can still fill retained transcript details after that
  // snapshot advances the global revision; `textDelta` is excluded because it
  // appends text and cannot be replayed safely.
  return isSessionDeltaEvent(delta) && delta.type !== "textDelta";
}

export function isDelegationDeltaEvent(delta: DeltaEvent): delta is DelegationDeltaEvent {
  return (
    delta.type === "delegationCreated" ||
    delta.type === "delegationWaitCreated" ||
    delta.type === "delegationWaitConsumed" ||
    delta.type === "delegationWaitResumeDispatchFailed" ||
    delta.type === "delegationUpdated" ||
    delta.type === "delegationCompleted" ||
    delta.type === "delegationFailed" ||
    delta.type === "delegationCanceled"
  );
}

export function staleSendRecoveryPollSessionIdsForDelta(delta: DeltaEvent) {
  if (isSessionDeltaEvent(delta)) {
    return [delta.sessionId];
  }
  if (delta.type === "orchestratorsUpdated" && delta.sessions?.length) {
    return delta.sessions.map((session) => session.id);
  }
  return [];
}
