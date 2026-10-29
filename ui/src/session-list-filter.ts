import type { RunningRunsByOwner } from "./test-runs";
import type { Session } from "./types";
import { sessionWorkActivity } from "./session-work-activity";

export type SessionListFilter = "all" | "working" | "asking" | "completed" | "error";

export type SessionListFilterCounts = Record<SessionListFilter, number>;

/** How many working sessions have a foreground turn, and how many own
 * background work. A session with both counts in each, once. */
export type SessionWorkingBreakdown = { foreground: number; background: number };

export function isSessionVisibleInSessionList(session: Session): boolean {
  return !session.parentDelegationId;
}

export function filterSessionListVisibleSessions(
  sessions: readonly Session[],
): Session[] {
  return sessions.filter(isSessionVisibleInSessionList);
}

// `runsByOwner` holds each session's running owned launcher work. A session is
// working while it has a foreground turn or owns such work, and an idle session
// that owns it is not completed. Without it, only the session status counts.
export function matchesSessionListFilter(
  session: Session,
  filter: SessionListFilter,
  runsByOwner?: RunningRunsByOwner,
) {
  switch (filter) {
    case "working": {
      const activity = sessionWorkActivity(session, runsByOwner);
      return activity.foreground || activity.background;
    }
    case "asking":
      return session.status === "approval";
    case "completed":
      return session.status === "idle" && !sessionWorkActivity(session, runsByOwner).background;
    case "error":
      return session.status === "error";
    case "all":
    default:
      return true;
  }
}

export function filterSessionsByListFilter(
  sessions: Session[],
  filter: SessionListFilter,
  runsByOwner?: RunningRunsByOwner,
): Session[] {
  if (filter === "all") {
    return sessions;
  }

  return sessions.filter((session) => matchesSessionListFilter(session, filter, runsByOwner));
}

export function countSessionsByFilter(
  sessions: Session[],
  runsByOwner?: RunningRunsByOwner,
): SessionListFilterCounts {
  const counts: SessionListFilterCounts = {
    all: sessions.length,
    working: 0,
    asking: 0,
    completed: 0,
    error: 0,
  };

  for (const session of sessions) {
    // Working is the union of foreground turns and owned background work, so
    // a session asking for approval or in error can also be working.
    if (matchesSessionListFilter(session, "working", runsByOwner)) counts.working += 1;
    switch (session.status) {
      case "approval":
        counts.asking += 1;
        break;
      case "idle":
        if (matchesSessionListFilter(session, "completed", runsByOwner)) counts.completed += 1;
        break;
      case "error":
        counts.error += 1;
        break;
    }
  }

  return counts;
}

export function countWorkingBreakdown(
  sessions: readonly Session[],
  runsByOwner?: RunningRunsByOwner,
): SessionWorkingBreakdown {
  const breakdown: SessionWorkingBreakdown = { foreground: 0, background: 0 };
  for (const session of sessions) {
    const activity = sessionWorkActivity(session, runsByOwner);
    if (activity.foreground) breakdown.foreground += 1;
    if (activity.background) breakdown.background += 1;
  }
  return breakdown;
}
