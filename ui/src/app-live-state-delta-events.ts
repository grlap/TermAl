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

// Every DeltaEvent type is classified once: tsc rejects a missing or unknown
// key. "session" types go through session replay, stale-send poll cancellation
// and hydration recovery; "global" types are routed by their own handlers, even
// when they carry a top-level sessionId (the test-run wait events do).
const DELTA_EVENT_SCOPE = {
  sessionCreated: "session",
  messageCreated: "session",
  messageUpdated: "session",
  textDelta: "session",
  textReplace: "session",
  commandUpdate: "session",
  parallelAgentsUpdate: "session",
  testRunCardUpdated: "session",
  conversationMarkerCreated: "session",
  conversationMarkerUpdated: "session",
  conversationMarkerDeleted: "session",
  codexUpdated: "global",
  orchestratorsUpdated: "global",
  testRunChanged: "global",
  testRunRemoved: "global",
  testRunWaitCreated: "global",
  testRunWaitConsumed: "global",
  testRunWaitResumeDispatchFailed: "global",
  delegationCreated: "global",
  delegationWaitCreated: "global",
  delegationWaitConsumed: "global",
  delegationWaitResumeDispatchFailed: "global",
  delegationUpdated: "global",
  delegationCompleted: "global",
  delegationFailed: "global",
  delegationCanceled: "global",
} as const satisfies Record<DeltaEvent["type"], "session" | "global">;

type SessionScopedDeltaType = {
  [K in keyof typeof DELTA_EVENT_SCOPE]: (typeof DELTA_EVENT_SCOPE)[K] extends "session" ? K : never;
}[keyof typeof DELTA_EVENT_SCOPE];
type SameTypes<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;
type AssertTrue<T extends true> = T;
// Compile-time check: the table and SessionDeltaEvent (live-updates.ts) agree.
export type DeltaScopeMatchesSessionDeltaEvent = AssertTrue<
  SameTypes<SessionScopedDeltaType, SessionDeltaEvent["type"]>
>;

export function isSessionDeltaEvent(delta: DeltaEvent): delta is SessionDeltaEvent {
  const scope = (DELTA_EVENT_SCOPE as Readonly<Record<string, "session" | "global" | undefined>>)[delta.type];
  if (scope !== undefined) return scope === "session";
  // A type this UI does not know yet (from a newer backend) keeps the
  // structural rule, so a sessionId still leads to the safe session resync.
  return "sessionId" in delta && typeof (delta as { sessionId?: unknown }).sessionId === "string";
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

export type TestRunWaitRecordDeltaEvent = Extract<
  DeltaEvent,
  { type: "testRunWaitCreated" | "testRunWaitConsumed" }
>;

// Deltas that carry only part of their commit. A delegation or test-run wait
// delta updates its own list by id and asks for an authoritative /api/state
// repair; it never moves the global revision, because the snapshot of the
// same commit (queued prompts, sibling waits) may not have arrived yet.
export function isAuthoritativeRepairDeltaEvent(
  delta: DeltaEvent,
): delta is DelegationDeltaEvent | TestRunWaitRecordDeltaEvent {
  return (
    isDelegationDeltaEvent(delta) ||
    delta.type === "testRunWaitCreated" ||
    delta.type === "testRunWaitConsumed"
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
