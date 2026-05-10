import type { Session } from "./types";

export type SessionListFilter = "all" | "working" | "asking" | "completed";

export type SessionListFilterCounts = Record<SessionListFilter, number>;

export function isSessionVisibleInSessionList(session: Session): boolean {
  return !session.parentDelegationId;
}

export function filterSessionListVisibleSessions(
  sessions: readonly Session[],
): Session[] {
  return sessions.filter(isSessionVisibleInSessionList);
}

export function matchesSessionListFilter(session: Session, filter: SessionListFilter) {
  switch (filter) {
    case "working":
      return session.status === "active";
    case "asking":
      return session.status === "approval";
    case "completed":
      return session.status === "idle";
    case "all":
    default:
      return true;
  }
}

export function filterSessionsByListFilter(
  sessions: Session[],
  filter: SessionListFilter,
): Session[] {
  if (filter === "all") {
    return sessions;
  }

  return sessions.filter((session) => matchesSessionListFilter(session, filter));
}

export function countSessionsByFilter(sessions: Session[]): SessionListFilterCounts {
  const counts: SessionListFilterCounts = {
    all: sessions.length,
    working: 0,
    asking: 0,
    completed: 0,
  };

  for (const session of sessions) {
    switch (session.status) {
      case "active":
        counts.working += 1;
        break;
      case "approval":
        counts.asking += 1;
        break;
      case "idle":
        counts.completed += 1;
        break;
      case "error":
        break;
    }
  }

  return counts;
}
