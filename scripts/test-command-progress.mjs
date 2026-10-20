// Inactivity is information, never permission to kill, retry or declare PASS.
import { statSync } from "node:fs";
export const FOCUSED_STALL_AFTER_MS = 300000;
export function watchCommandProgress({ log, pid, runId, stage, observe }, {
  now = () => performance.now(), stallAfterMs = FOCUSED_STALL_AFTER_MS, pollMs = 1000,
  onObserverError = (error) => console.error(`progress observation unavailable: ${error.message}`),
} = {}) {
  if (!(stallAfterMs > 0) || !(pollMs > 0)) throw new Error("progress intervals must be positive");
  let lastSize = statSync(log).size, lastProgress = now(), stalled = false, episode = 0;
  let pendingResume, lastReadError;
  const report = (observation) => {
    try { observe(observation); return true; }
    catch (error) {
      // A failed save must not crash the launcher or recursively call observe.
      try { onObserverError(error); } catch { /* Diagnostic sinks are best effort. */ }
      return false;
    }
  };
  const timer = setInterval(() => {
    try {
      const size = statSync(log).size;
      lastReadError = undefined;
      const current = now();
      if (size !== lastSize) {
        lastSize = size; lastProgress = current;
        if (stalled) pendingResume ??= { state: "resumed", runId, stage, pid, log, inactiveMs: 0, episode };
      }
      // Log progress and durable delivery are different state transitions.
      // Retry a failed resume even when no additional output arrives.
      if (pendingResume && report(pendingResume)) { pendingResume = undefined; stalled = false; }
      if (!stalled && current - lastProgress >= stallAfterMs) {
        const nextEpisode = episode + 1;
        if (report({ state: "stalled", runId, stage, pid, log, inactiveMs: current - lastProgress, episode: nextEpisode })) {
          stalled = true; episode = nextEpisode;
        }
      }
    } catch (error) {
      if (lastReadError !== error.message && report({ state: "unknown", runId, stage, pid, log, error: error.message, episode })) {
        lastReadError = error.message;
      }
    }
  }, pollMs);
  return () => clearInterval(timer);
}
