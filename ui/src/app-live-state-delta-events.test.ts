// Owns runtime tests for the delta-scope table in app-live-state-delta-events.ts.
// Completeness and agreement with SessionDeltaEvent are checked by tsc there.
import { expect, it } from "vitest";
import { isSessionDeltaEvent } from "./app-live-state-delta-events";
import type { DeltaEvent } from "./types";

it("routes session deltas and keeps sessionId-bearing global deltas out of session handling", () => {
  const messageCreated = { type: "messageCreated", revision: 2, sessionId: "s" } as unknown as DeltaEvent;
  const markerDeleted = { type: "conversationMarkerDeleted", revision: 2, sessionId: "s", markerId: "m" } as unknown as DeltaEvent;
  expect(isSessionDeltaEvent(messageCreated)).toBe(true);
  expect(isSessionDeltaEvent(markerDeleted)).toBe(true);
  for (const type of ["testRunWaitConsumed", "testRunWaitResumeDispatchFailed"]) {
    expect(isSessionDeltaEvent({ type, revision: 2, sessionId: "s" } as unknown as DeltaEvent)).toBe(false);
  }
  expect(isSessionDeltaEvent({ type: "codexUpdated", revision: 2 } as unknown as DeltaEvent)).toBe(false);
});

it("keeps the structural rule for delta types this UI does not know yet", () => {
  // A newer backend's session-scoped event still reaches the safe session resync.
  expect(isSessionDeltaEvent({ type: "futureSessionEvent", revision: 3, sessionId: "s" } as unknown as DeltaEvent)).toBe(true);
  expect(isSessionDeltaEvent({ type: "futureGlobalEvent", revision: 3 } as unknown as DeltaEvent)).toBe(false);
});

it.each(["toString", "constructor", "__proto__"])("treats inherited object member %s as an unknown wire type", type => {
  expect(isSessionDeltaEvent({ type, revision: 3, sessionId: "s" } as unknown as DeltaEvent)).toBe(true);
  expect(isSessionDeltaEvent({ type, revision: 3 } as unknown as DeltaEvent)).toBe(false);
});
