// Read-only launcher discovery contract and presentation rules. Does not own
// execution, notification delivery, or agent lifecycle state.
export type TestRunState = "running" | "passed" | "failed" | "unknown";
export type TestRunUnknownReason = "processGone" | "resultsUnreadable" | "noPid";
export type TestRunStageState = "unrun" | "running" | "passed" | "failed";
export interface TestRunStageSummary {
  name: string;
  state: TestRunStageState;
  exitCode: number | null;
  startedAt: string | null;
  endedAt: string | null;
}
export interface TestRunSummary {
  runId: string;
  /** Opaque backend detail identity; older hosts may omit it. */
  detailVersion?: string | null;
  projectId: string | null;
  worktree: string;
  runDir: string;
  preset: "full" | "focused" | "live";
  command: string[] | null;
  commandTruncated: boolean;
  detached: boolean | null;
  state: TestRunState;
  /** Only supplied for unknown runs; absence does not imply any cause. */
  unknownReason?: TestRunUnknownReason;
  interrupted: boolean;
  currentStage: string | null;
  stages: TestRunStageSummary[];
  ownerSessionId: string | null;
  notifyTo: string | null;
  notifySessionId: string | null;
  startedAt: string | null;
  endedAt: string | null;
  exitCode: number | null;
  error: string | null;
}
export interface TestRunDiagnostics { text: string; truncated: boolean }
export interface TestRunStageDetail extends TestRunStageSummary {
  command: string[] | null;
  cwd: string | null;
  log: string | null;
  diagnostics: TestRunDiagnostics | null;
  error: string | null;
}
export interface TestRunPreflight {
  name: string;
  command: string[] | null;
  exitCode: number | null;
  log: string | null;
  diagnostics: TestRunDiagnostics | null;
}
export interface TestRunDetail {
  stages: TestRunStageDetail[];
  preflight: TestRunPreflight[];
  expectedFingerprint: string | null;
  before: string | null;
  after: string | null;
  limitations: string | null;
}
export type TestRunDelta =
  | { type: "testRunChanged"; revision: number; run: TestRunSummary }
  | { type: "testRunRemoved"; revision: number; runId: string };

export function sortTestRuns(runs: readonly TestRunSummary[]): TestRunSummary[] {
  return [...runs].sort((a, b) => (b.startedAt ?? "").localeCompare(a.startedAt ?? "") || a.runId.localeCompare(b.runId));
}
export function reconcileTestRunSnapshot(previous: TestRunSummary[], incoming: readonly TestRunSummary[]) {
  const byId = new Map(previous.map(run => [run.runId, run]));
  const next = sortTestRuns(incoming).map(run => {
    const old = byId.get(run.runId);
    // Without a detail version an equal summary cannot prove that diagnostics
    // are unchanged. Preserve conservative invalidation for older hosts.
    return old && run.detailVersion && sameWireValue(old, run) ? old : run;
  });
  return next.length === previous.length && next.every((run, i) => run === previous[i]) ? previous : next;
}

// Wire values are JSON trees; property order is not part of their identity.
function sameWireValue(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (!a || !b || typeof a !== "object" || typeof b !== "object") return false;
  if (Array.isArray(a) || Array.isArray(b)) return Array.isArray(a) && Array.isArray(b)
    && a.length === b.length && a.every((value, i) => sameWireValue(value, b[i]));
  const left = a as Record<string, unknown>, right = b as Record<string, unknown>;
  const keys = Object.keys(left);
  return keys.length === Object.keys(right).length && keys.every(key => Object.prototype.hasOwnProperty.call(right, key) && sameWireValue(left[key], right[key]));
}
export function applyTestRunDelta(runs: readonly TestRunSummary[], event: TestRunDelta) {
  const id = event.type === "testRunChanged" ? event.run.runId : event.runId;
  const next = runs.filter(run => run.runId !== id);
  // Adopt equal summaries too: each changed event also invalidates detail.
  if (event.type === "testRunChanged") next.push(event.run);
  return sortTestRuns(next);
}
export function testRunMatchesSession(run: TestRunSummary, sessionId: string) {
  return run.ownerSessionId === sessionId || run.notifySessionId === sessionId;
}
export function testRunSessionMarker(runs: readonly TestRunSummary[], sessionId: string) {
  const relevant = runs.filter(run => run.state === "running" && testRunMatchesSession(run, sessionId));
  if (!relevant.length) return null;
  const foreground = relevant.some(run => run.ownerSessionId === sessionId && run.detached === false);
  const owned = relevant.some(run => run.ownerSessionId === sessionId);
  const label = foreground ? "running tests" : owned ? "test run in background" : "test run will notify this session";
  const roleRuns = relevant.filter(run => foreground
    ? run.ownerSessionId === sessionId && run.detached === false
    : owned ? run.ownerSessionId === sessionId : run.notifySessionId === sessionId);
  if (roleRuns.length > 1) return `${label} (${roleRuns.length})`;
  const suffix = roleRuns[0].currentStage;
  return suffix ? `${label} · ${suffix}` : label;
}
export function testRunStageLabel(run: TestRunSummary, stage: TestRunStageSummary) {
  return stage.state === "running" && (run.state === "unknown" || run.interrupted) ? "interrupted" : stage.state;
}
