// Run-wait visibility for pane and board. Joins registered waits to live run
// summaries, never infers a wait from a card or a notification recipient.
import { createContext, useContext, useEffect, useState } from "react";
import { testRunWaitPrompt, type TestRunWaitRecord, type TestRunWaitFailures } from "./test-run-waits";
import type { TestRunSummary } from "./test-runs";
import type { SessionStatus } from "./types";
import type { TestRunViewRevision } from "./test-run-overview";
import { readTestRun } from "./test-runs-api";

export const EMPTY_TEST_RUN_WAITS: {
  waits: readonly TestRunWaitRecord[]; runs: readonly TestRunSummary[];
  viewRevision?: TestRunViewRevision;
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
  const { waits, runs, missingStatus } = useVisibleWaitRuns(sessionId, status === "idle");
  const [now, setNow] = useState(Date.now);
  const visible = status === "idle" && !!sessionId && waits.some(wait => wait.sessionId === sessionId);
  useEffect(() => {
    if (!visible) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [visible]);
  return {
    prompt: visible ? testRunWaitPrompt(waits, runs, sessionId!, now, missingStatus) : null,
    announcement: visible ? testRunWaitPrompt(waits, runs, sessionId!, undefined, missingStatus) : null,
  };
}

// Timer-free form for consumers that never show elapsed time, such as the
// aria-hidden transcript activity slot. Visible under the same rule as above.
export function useTestRunWaitAnnouncement(sessionId: string | undefined, status: SessionStatus | undefined) {
  const { waits, runs, missingStatus } = useVisibleWaitRuns(sessionId, status === "idle");
  return status === "idle" && sessionId ? testRunWaitPrompt(waits, runs, sessionId, undefined, missingStatus) : null;
}

// Read only identities named by a mounted, visible wait. No history scan or
// global detail cache; aborting the effect rejects a prior revision's reply.
function useVisibleWaitRuns(sessionId: string | undefined, visible: boolean) {
  const context = useContext(TestRunWaitsContext);
  const { waits, viewRevision } = context;
  const idsKey = JSON.stringify(visible && sessionId
    ? [...new Set(waits.filter(wait => wait.sessionId === sessionId).flatMap(wait => wait.runIds))] : []);
  const [read, setRead] = useState<{ idsKey: string; revision: TestRunViewRevision;
    runs: TestRunSummary[]; done: boolean }>();
  useEffect(() => {
    if (!viewRevision || idsKey === "[]") return;
    const controller = new AbortController();
    const ids = JSON.parse(idsKey) as string[];
    const rows: TestRunSummary[] = [];
    let cursor = 0;
    const worker = async () => {
      while (!controller.signal.aborted && cursor < ids.length) {
        const id = ids[cursor++];
        try {
          const result = await readTestRun(id, controller.signal);
          if (!controller.signal.aborted) rows.push(result.run);
        } catch { /* A missing detail never means the registered wait ended. */ }
      }
    };
    void Promise.all(Array.from({ length: Math.min(4, ids.length) }, worker)).then(() => {
      if (!controller.signal.aborted) setRead({ idsKey, revision: viewRevision, runs: rows, done: true });
    });
    return () => controller.abort();
  }, [idsKey, viewRevision]);
  const current = read?.idsKey === idsKey && read.revision === viewRevision ? read : undefined;
  // A refresh is not evidence that the last observed status disappeared.
  // Keep its uncertainty and announcement stable within the same instance.
  const readable = read?.idsKey === idsKey && read.revision.serverInstanceId === viewRevision?.serverInstanceId ? read : undefined;
  return { waits, runs: viewRevision ? readable?.runs ?? [] : context.runs,
    missingStatus: viewRevision ? current?.done ? "status unavailable" : "loading status" : "not indexed" };
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
