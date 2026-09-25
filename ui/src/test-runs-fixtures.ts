// Shared test-run wire fixtures. No launcher or backend processes are started.
import type { TestRunDetail, TestRunSummary } from "./test-runs";

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
