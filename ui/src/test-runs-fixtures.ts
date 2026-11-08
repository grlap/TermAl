// Shared test-run wire fixtures. No launcher or backend processes are started.
import type { TestRunDetail, TestRunSummary } from "./test-runs";
import type { TestRunCardSnapshot } from "./test-run-card";
import type { TestRunWaitRecord } from "./test-run-waits";
import type { TestRunOverview, SessionTestRunActivity } from "./test-run-overview";

export function makeTestRunOverview(overrides: Partial<TestRunOverview> = {}): TestRunOverview {
  return { version: 1, ready: true, total: 0, running: 0, passed: 0, failed: 0, unknown: 0,
    needsAttention: 0, evidenceAtRisk: 0, evidenceSafe: 0, evidenceUnknown: 0, projects: [], ...overrides };
}
export function makeTestRunActivity(overrides: Partial<SessionTestRunActivity> = {}): SessionTestRunActivity {
  return { ownedRunning: 0, foregroundRunning: 0, backgroundRunning: 0, modeUnknownRunning: 0, notifyRunning: 0,
    foregroundStage: null, ownedStage: null, notifyStage: null, ...overrides };
}

export function makeTestRunCard(overrides: Partial<TestRunCardSnapshot> = {}): TestRunCardSnapshot {
  return { ...makeTestRun(), stagesOmitted: 0, errorTruncated: false, failure: null, ...overrides };
}

export function makeTestRunWait(overrides: Partial<TestRunWaitRecord> = {}): TestRunWaitRecord {
  const run = makeTestRun();
  return { id: "wait-one", sessionId: "session-coordinator", runIds: [run.runId], mode: "all",
    createdAt: run.startedAt!, runs: [{ runId: run.runId, runDir: run.runDir, preset: run.preset,
      worktree: run.worktree, ownerSessionId: run.ownerSessionId, startedAt: run.startedAt }], ...overrides };
}

export function makeTestRun(overrides: Partial<TestRunSummary> = {}): TestRunSummary {
  return {
    runId: "test-one", projectId: "project-one", worktree: "C:/repo", runDir: "C:/repo/.git/review-runs/test-one",
    preset: "full", command: null, commandTruncated: false, detached: false, detailVersion: "detail-v1",
    state: "running", interrupted: false, currentStage: "rust-tests", stages: [],
    ownerSessionId: "session-owner", notifyTo: "Coordinator", notifySessionId: "session-coordinator",
    startedAt: "2026-09-25T12:00:00.000Z", endedAt: null, exitCode: null, error: null,
    ...overrides,
  };
}
export function makeTestRunDetail(overrides: Partial<TestRunDetail> = {}): TestRunDetail {
  return {
    stages: [{ name: "rust-tests", state: "running", exitCode: null,
      startedAt: "2026-09-25T12:00:00.000Z", endedAt: null,
      command: ["cargo", "test"], cwd: "C:/repo", log: "rust-tests.stdout.log", diagnostics: null, error: null }],
    preflight: [], expectedFingerprint: "fingerprint", before: "fingerprint", after: null, limitations: null,
    ...overrides,
  };
}
