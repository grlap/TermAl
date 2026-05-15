// app-live-state-activity.ts
//
// Owns pure live-transport and resume-watchdog activity map updates for
// `useAppLiveState`.
//
// Does not own: reconnect/retry timers, backend-connection UI state,
// EventSource lifecycle, or request scheduling.
//
// Split out of: ui/src/app-live-state.ts. Keep this module free of React hook
// state; callers own when activity updates should reset retry cooldowns.

import type { Session } from "./types";

export function markLiveTransportActivity(
  activityBySessionId: Map<string, number>,
  sessionIds: Iterable<string>,
  now: number,
) {
  for (const sessionId of sessionIds) {
    activityBySessionId.set(sessionId, now);
  }
}

export function syncLiveTransportActivityFromState(
  activityBySessionId: Map<string, number>,
  sessions: Session[],
  now: number,
) {
  // Snapshot adoption seeds the baseline for every listed session immediately.
  // Idle entries are harmless because stale-transport checks still gate on
  // session.status === "active".
  markLiveTransportActivity(
    activityBySessionId,
    sessions.map((session) => session.id),
    now,
  );
}

export function markLiveSessionResumeWatchdogBaseline(
  baselineBySessionId: Map<string, number>,
  sessionIds: Iterable<string>,
  now: number,
) {
  // Data-bearing live events must advance this baseline for their sessions.
  // Otherwise the resume watchdog can interpret ordinary long-running streams
  // as a wake gap and poll /api/state until unrelated reconnect activity clears
  // the cooldown. See docs/architecture.md "Live-state reconnect and watchdog
  // recovery".
  for (const sessionId of sessionIds) {
    baselineBySessionId.set(sessionId, now);
  }
}

export function pruneLiveSessionResumeWatchdogBaselineSessions(
  baselineBySessionId: Map<string, number>,
  sessions: Session[],
) {
  const liveSessionIds = new Set(sessions.map((session) => session.id));
  for (const sessionId of baselineBySessionId.keys()) {
    if (!liveSessionIds.has(sessionId)) {
      baselineBySessionId.delete(sessionId);
    }
  }
}

export function syncLiveSessionResumeWatchdogBaselines(
  baselineBySessionId: Map<string, number>,
  sessions: Session[],
  now: number,
) {
  // Advance every currently known session so idle-to-active transitions do not
  // inherit a false wake gap from time spent without live streaming.
  markLiveSessionResumeWatchdogBaseline(
    baselineBySessionId,
    sessions.map((session) => session.id),
    now,
  );
  pruneLiveSessionResumeWatchdogBaselineSessions(baselineBySessionId, sessions);
}
