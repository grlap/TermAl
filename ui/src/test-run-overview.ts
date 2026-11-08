// Bounded public test-run values. The backend owns canonical history and
// counters; these adapters only present admitted snapshots and invalidations.
import type { TestRunSummary } from "./test-runs";

export interface TestRunCounts {
  total: number; running: number; passed: number; failed: number; unknown: number;
  needsAttention: number; evidenceAtRisk: number; evidenceSafe: number; evidenceUnknown: number;
}
export interface TestRunOverview extends TestRunCounts {
  version: number;
  ready: boolean;
  projects: (TestRunCounts & { projectId: string | null })[];
}
export interface SessionTestRunActivity {
  ownedRunning: number; foregroundRunning: number; backgroundRunning: number;
  modeUnknownRunning: number; notifyRunning: number;
  foregroundStage: string | null; ownedStage: string | null; notifyStage: string | null;
}
export type TestRunActivities = Readonly<Record<string, SessionTestRunActivity>>;
export interface TestRunViewRevision {
  serverInstanceId: string;
  revision: number;
  version: number;
}
export type TestRunsInvalidated = {
  type: "testRunsInvalidated";
  revision: number;
  serverInstanceId: string;
  overview: TestRunOverview;
  activity: TestRunActivities;
};
export type SessionWorkByOwner = ReadonlyMap<string, readonly TestRunSummary[] | SessionTestRunActivity>;
export const NO_TEST_RUN_ACTIVITIES: TestRunActivities = {};

export function testRunActivityLabel(activity: SessionTestRunActivity | undefined) {
  if (!activity || !(activity.ownedRunning || activity.notifyRunning)) return null;
  const foreground = activity.foregroundRunning > 0;
  const owned = activity.ownedRunning > 0;
  const label = foreground ? "running tests" : owned
    ? activity.modeUnknownRunning ? "test running" : "test run in background"
    : "test run will notify this session";
  const count = foreground ? activity.foregroundRunning : owned ? activity.ownedRunning : activity.notifyRunning;
  const stage = foreground ? activity.foregroundStage : owned ? activity.ownedStage : activity.notifyStage;
  return count > 1 ? `${label} (${count})` : stage ? `${label} · ${stage}` : label;
}

// Compatibility for hosts that still send run summaries. Both wire versions
// use the same role/count presentation rather than making up run identities.
export function activityFromRuns(runs: readonly TestRunSummary[], sessionId: string): SessionTestRunActivity {
  const owned = runs.filter(run => run.state === "running" && run.ownerSessionId === sessionId);
  const foreground = owned.filter(run => run.detached === false);
  const notify = runs.filter(run => run.state === "running" && run.notifySessionId === sessionId && run.ownerSessionId !== sessionId);
  return {
    ownedRunning: owned.length, foregroundRunning: foreground.length,
    backgroundRunning: owned.length - foreground.length,
    modeUnknownRunning: owned.filter(run => run.detached == null && run.registrationState === "hostObserved").length,
    notifyRunning: notify.length,
    foregroundStage: foreground.length === 1 ? foreground[0].currentStage : null,
    ownedStage: owned.length === 1 ? owned[0].currentStage : null,
    notifyStage: notify.length === 1 ? notify[0].currentStage : null,
  };
}
