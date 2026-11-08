// What a session in the Sessions list is busy with: a foreground turn, owned
// launcher work running in the background, or both. session-list-filter.ts
// counts and filters with it; AppControlSurface.tsx uses it for busy badges.
// Does not own: run state or ownership (test-runs.ts), the session's lifecycle
// status (the host's), or which runs exist (live state). Only runs the test-run
// index knows about count; a build that was never registered there is not
// seen.
// New file; not split from an existing one.
import { testRunDetachModeUnobserved } from "./test-runs";
import type { SessionWorkByOwner, SessionTestRunActivity } from "./test-run-overview";
import type { Session } from "./types";

export type SessionWorkActivity = {
  /** The session's own turn is running (active or stopping). */
  foreground: boolean;
  /** The session owns running work beyond its foreground turn. */
  background: boolean;
  /** That background work is concurrent with a turn, and the host observed
   * the run's command without learning whether it was detached: the same
   * rule as the session pane's marker (test-runs.ts testRunSessionMarker). */
  modeUnknown: boolean;
};

export function hasForegroundTurn(session: Session): boolean {
  return session.status === "active" || session.status === "stopping";
}

export function sessionWorkActivity(
  session: Session,
  runsByOwner: SessionWorkByOwner | undefined,
): SessionWorkActivity {
  const foreground = hasForegroundTurn(session);
  const owned = runsByOwner?.get(session.id) ?? [];
  if (!Array.isArray(owned)) {
    const activity = owned as SessionTestRunActivity;
    return { foreground,
      background: (foreground ? activity.backgroundRunning : activity.ownedRunning) > 0,
      modeUnknown: foreground && activity.backgroundRunning > 0 && activity.modeUnknownRunning === activity.backgroundRunning };
  }
  // During a turn, a run the turn itself started in the foreground is part of
  // that turn. Without a turn, any running owned run is background work,
  // whatever its detach mode.
  const background = foreground ? owned.filter((run) => run.detached !== false) : owned;
  return {
    foreground,
    background: background.length > 0,
    modeUnknown: foreground && background.length > 0 && background.every(testRunDetachModeUnobserved),
  };
}
