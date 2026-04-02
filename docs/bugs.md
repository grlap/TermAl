# Bugs & Known Issues

Updated against the current checked-in code in `src/main.rs`, `ui/src/App.tsx`,
`ui/package.json`, and `ui/vite.config.ts`.

Completed items are removed from this file once fixed. The sections below track
active bugs and follow-up tasks only.

## Active-session resume recovery still misses monitor-off / no-lifecycle wake paths

**Severity:** Medium - completed replies can remain hidden after display sleep or monitor power-off, so users may send duplicate prompts before the UI catches up.

The current `App.tsx` change improves one class of stale live-turn recovery by forcing a state resync when a hidden page becomes visible again. That is a useful guard for tab/background suspension, but it still assumes the browser emits `visibilitychange`, `focus`, `pageshow`, or related lifecycle events when the machine wakes back up.

The user-reported failure mode is narrower and different: the monitor can power off while the TermAl window remains the active foreground window. In that case the page may resume with no visibility or focus transition at all, which means the new logic never calls `requestStateResync(...)`. The last assistant reply can still stay hidden behind the stale live-turn card until some later action forces a refresh.

**Current behavior:**

- active-session recovery now depends on `blur`/`focus`, `pagehide`/`pageshow`, or `visibilitychange`
- if the display sleeps or is powered off without producing those events, the UI can miss the completion and continue showing the stale live-turn placeholder
- the current regression only covers the synthetic `visibilitychange` path, not the no-lifecycle monitor-off path the user reported

**Proposal:**

- add a recovery path that does not depend solely on page visibility or focus transitions, such as a transport-staleness watchdog or explicit reconnect/resume probe for live sessions
- make the stale-live-session heuristic trigger on wake/resume conditions even when the page never became hidden
- keep the current visibility-based regression, but add coverage for the no-lifecycle wake path that was still left open

## P2

- [ ] Add a frontend regression for stale live sessions after wake without visibility events:
      simulate a live session whose final state is missed while the window stays focused, then
      verify TermAl reveals the completed reply without requiring another prompt or a synthetic
      `visibilitychange` event.
