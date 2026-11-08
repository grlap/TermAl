// App-scoped projection of revision-gated live state; no independent SSE or
// inference that a notification recipient is idle or registered as waiting.
import { createContext, useContext, useMemo, type ReactNode } from "react";
import { useStableEvent } from "./panels/use-stable-event";
import { testRunSessionMarker, type TestRunSummary } from "./test-runs";
import { NO_TEST_RUN_ACTIVITIES, testRunActivityLabel, type TestRunActivities, type TestRunOverview, type TestRunViewRevision } from "./test-run-overview";
import "./panels/test-runs-panel.css";
import type { TestRunCardTarget } from "./test-run-card";
import type { TestRunWaitRecord, TestRunWaitFailures } from "./test-run-waits";
import {
  EMPTY_TEST_RUN_WAITS, NO_TEST_RUN_WAITING_SESSIONS, TestRunWaitingSessionsContext, TestRunWaitsContext,
  TestRunWaitFailuresContext,
} from "./test-run-waits-context";

export type OpenTestRuns = (sessionId?: string | null, target?: TestRunCardTarget, allProjects?: boolean) => void;

export const TestRunsOpenContext = createContext<OpenTestRuns | null>(null);

export const TestRunsContext = createContext<{
  runs: readonly TestRunSummary[];
  open: OpenTestRuns | null;
  snapshotReady: boolean;
  overview?: TestRunOverview;
  activity?: TestRunActivities;
  viewRevision?: TestRunViewRevision;
}>({ runs: [], open: null, snapshotReady: false, activity: NO_TEST_RUN_ACTIVITIES });

const NO_WAITS: readonly TestRunWaitRecord[] = [];
const NO_FAILURES: TestRunWaitFailures = {};
const NO_DISMISS = () => {};

// snapshotReady is required: a default of true would let a new call site claim
// the index is ready and show "Run not currently indexed" before the first
// snapshot. The context itself defaults to false (not ready).
export function TestRunsProvider({ runs, overview, activity = NO_TEST_RUN_ACTIVITIES, viewRevision, waits = NO_WAITS, failures = NO_FAILURES, dismissFailure = NO_DISMISS, snapshotReady, open, children }: {
  overview?: TestRunOverview; activity?: TestRunActivities; viewRevision?: TestRunViewRevision;
  runs: readonly TestRunSummary[]; waits?: readonly TestRunWaitRecord[]; open: OpenTestRuns; children: ReactNode;
  failures?: TestRunWaitFailures; dismissFailure?: (sessionId: string) => void; snapshotReady: boolean;
}) {
  const stableOpen = useStableEvent(open);
  const value = useMemo(() => ({ runs, open: stableOpen, snapshotReady, overview, activity, viewRevision }),
    [runs, stableOpen, snapshotReady, overview, activity, viewRevision]);
  const waitValue = useMemo(() => waits.length ? { waits, runs, viewRevision } : EMPTY_TEST_RUN_WAITS, [waits, runs, viewRevision]);
  const waitingSessions = useMemo(
    () => waits.length ? new Set(waits.map(wait => wait.sessionId)) : NO_TEST_RUN_WAITING_SESSIONS,
    [waits],
  );
  const stableDismiss = useStableEvent(dismissFailure);
  const failureValue = useMemo(() => ({ failures, dismiss: stableDismiss }), [failures, stableDismiss]);
  return <TestRunsOpenContext.Provider value={stableOpen}>
    <TestRunWaitFailuresContext.Provider value={failureValue}>
    <TestRunWaitsContext.Provider value={waitValue}>
    <TestRunWaitingSessionsContext.Provider value={waitingSessions}>
      <TestRunsContext.Provider value={value}>{children}</TestRunsContext.Provider>
    </TestRunWaitingSessionsContext.Provider>
    </TestRunWaitsContext.Provider>
    </TestRunWaitFailuresContext.Provider>
  </TestRunsOpenContext.Provider>;
}

export function TestRunSessionMarker({ sessionId }: { sessionId: string }) {
  const { runs, open, overview, activity } = useContext(TestRunsContext);
  const label = overview ? testRunActivityLabel(activity?.[sessionId]) : testRunSessionMarker(runs, sessionId);
  if (!label) return null;
  return <button type="button" className="test-run-session-marker" onClick={event => {
    event.stopPropagation();
    open?.(sessionId);
  }} title={`${label} — Open test runs for this session`}>{label}</button>;
}
