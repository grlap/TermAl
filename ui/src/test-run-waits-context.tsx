// Run-wait visibility for pane and board. Joins registered waits to live run
// summaries, never infers a wait from a card or a notification recipient.
import { createContext, useContext, useEffect, useState } from "react";
import { testRunWaitPrompt, type TestRunWaitRecord, type TestRunWaitFailures } from "./test-run-waits";
import type { TestRunSummary } from "./test-runs";
import type { SessionStatus } from "./types";

export const EMPTY_TEST_RUN_WAITS: {
  waits: readonly TestRunWaitRecord[]; runs: readonly TestRunSummary[];
} = { waits: [], runs: [] };
export const TestRunWaitsContext = createContext(EMPTY_TEST_RUN_WAITS);

// The ids of sessions with a pending run wait, memoized on the waits alone, so
// a consumer that only asks "does this session wait?" does not re-render on
// every run-progress update.
export const NO_TEST_RUN_WAITING_SESSIONS: ReadonlySet<string> = new Set();
export const TestRunWaitingSessionsContext = createContext(NO_TEST_RUN_WAITING_SESSIONS);

export function useHasPendingTestRunWait(sessionId: string | undefined) {
  const waitingSessions = useContext(TestRunWaitingSessionsContext);
  return !!sessionId && waitingSessions.has(sessionId);
}

export const TestRunWaitFailuresContext = createContext<{
  failures: TestRunWaitFailures; dismiss: (sessionId: string) => void;
}>({ failures: {}, dismiss: () => {} });

export function useTestRunWaitPrompt(sessionId: string | undefined, status: SessionStatus) {
  const { waits, runs } = useContext(TestRunWaitsContext);
  const [now, setNow] = useState(Date.now);
  const visible = status === "idle" && !!sessionId && waits.some(wait => wait.sessionId === sessionId);
  useEffect(() => {
    if (!visible) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [visible]);
  return {
    prompt: visible ? testRunWaitPrompt(waits, runs, sessionId!, now) : null,
    announcement: visible ? testRunWaitPrompt(waits, runs, sessionId!) : null,
  };
}

// Timer-free form for consumers that never show elapsed time, such as the
// aria-hidden transcript activity slot. Visible under the same rule as above.
export function useTestRunWaitAnnouncement(sessionId: string | undefined, status: SessionStatus | undefined) {
  const { waits, runs } = useContext(TestRunWaitsContext);
  return status === "idle" && sessionId ? testRunWaitPrompt(waits, runs, sessionId) : null;
}

export function TestRunWaitIndicator({ sessionId, status }: { sessionId: string; status: SessionStatus }) {
  const { prompt } = useTestRunWaitPrompt(sessionId, status);
  return prompt ? <span className="test-run-wait-indicator" title={prompt}>{prompt}</span> : null;
}

// Kept outside the transcript and independent of transient connection health.
// Survives ordinary deltas/snapshots and tab changes until explicitly dismissed.
// Only one mounted notice per failure should announce: the pane toolbar does;
// the board passes announce={false} so screen readers hear it once.
export function TestRunWaitFailureNotice({ sessionId, announce = true }: { sessionId: string; announce?: boolean }) {
  const { failures, dismiss } = useContext(TestRunWaitFailuresContext);
  const failure = failures[sessionId];
  if (!failure) return null;
  return <details className="test-run-wait-failure">
    <summary><span role={announce ? "status" : undefined}>Test-run resume failed</span></summary>
    <p title={failure.error}>{failure.error}</p>
    <button type="button" onClick={event => { event.stopPropagation(); dismiss(sessionId); }}
      aria-label="Dismiss test-run resume failure">Dismiss</button>
  </details>;
}
