// tail-read-backstop-schedule.ts
//
// Owns: the per-session spacing of the periodic tail-read backstop while the
// same tail demand stands. The first backstop read goes out on the backstop's
// normal 3 s cadence; each later one waits twice as long as the one before,
// up to a 60 s cap. A session starts again at 3 s once its demand has gone
// (its tail certified), once another path raises a read for it (new demand),
// or after it has left the visible set.
//
// Does not own: whether a session needs a tail read (the transcript repair
// authority), issuing the read (app-live-state.ts), or the 1 s tick and its
// 3-tick boundary (app-live-state-transport.ts). Edge-triggered tail reads do
// not pass through this schedule.

export const TAIL_READ_BACKSTOP_INITIAL_DELAY_MS = 3_000;
export const TAIL_READ_BACKSTOP_MAX_DELAY_MS = 60_000;

// Browser interval firings can land slightly before the exact due time; a
// read due within this margin is treated as due rather than skipped for a
// whole extra 3 s boundary.
const DUE_TOLERANCE_MS = 500;

type ScheduleEntry = { nextDueAt: number; delayMs: number };

export class TailReadBackstopSchedule {
  private readonly entries = new Map<string, ScheduleEntry>();

  /** Whether a backstop read for this session may go out at `now`. */
  isDue(sessionId: string, now: number): boolean {
    const entry = this.entries.get(sessionId);
    return entry === undefined || now + DUE_TOLERANCE_MS >= entry.nextDueAt;
  }

  /** Records a backstop read issued at `now` and spaces out the next one. */
  recordIssued(sessionId: string, now: number) {
    const previous = this.entries.get(sessionId);
    const delayMs = previous === undefined
      ? TAIL_READ_BACKSTOP_INITIAL_DELAY_MS
      : Math.min(previous.delayMs * 2, TAIL_READ_BACKSTOP_MAX_DELAY_MS);
    this.entries.set(sessionId, { nextDueAt: now + delayMs, delayMs });
  }

  /** Forgets the session's spacing once its demand has gone or is new. */
  clear(sessionId: string) {
    this.entries.delete(sessionId);
  }

  /** Forgets the spacing of every session not in `sessionIds`. */
  retainOnly(sessionIds: ReadonlySet<string>) {
    for (const sessionId of this.entries.keys()) {
      if (!sessionIds.has(sessionId)) this.entries.delete(sessionId);
    }
  }
}
