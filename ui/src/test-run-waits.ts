// Registered run waits, independent of run ownership and notification targets.
// Pure projection only: no polling, transcript entries, or inferred waits.
import type { TestRunSummary } from "./test-runs";
import { testRunUnknownLabel } from "./test-run-card";

export interface TestRunWaitRecord {
  id: string;
  sessionId: string;
  runIds: string[];
  mode: "all" | "any";
  createdAt: string;
  title?: string | null;
  runs: { runId: string; runDir: string; preset: "full" | "focused" | "live"; worktree: string; ownerSessionId: string | null; startedAt: string | null }[];
}
export type TestRunWaitDelta =
  | { type: "testRunWaitCreated"; revision: number; serverInstanceId: string; wait: TestRunWaitRecord }
  | { type: "testRunWaitConsumed"; revision: number; serverInstanceId: string; waitId: string; sessionId: string;
      reason: "completed" | "sessionStopped" | "sessionUnavailable" | "sessionRemoved" }
  // serverInstanceId is absent from backends before the field was added.
  | { type: "testRunWaitResumeDispatchFailed"; revision: number; sessionId: string; error: string; serverInstanceId?: string };

export type TestRunWaitFailure = Extract<TestRunWaitDelta, { type: "testRunWaitResumeDispatchFailed" }>;
export type TestRunWaitFailures = Readonly<Record<string, TestRunWaitFailure>>;

export function applyTestRunWaitDelta(waits: readonly TestRunWaitRecord[], delta: TestRunWaitDelta) {
  if (delta.type === "testRunWaitResumeDispatchFailed") return waits;
  if (delta.type === "testRunWaitConsumed") return waits.filter(wait => wait.id !== delta.waitId);
  return [...waits.filter(wait => wait.id !== delta.wait.id), delta.wait];
}

export function testRunWaitPrompt(waits: readonly TestRunWaitRecord[], runs: readonly TestRunSummary[], sessionId: string, now?: number) {
  const own = waits.filter(wait => wait.sessionId === sessionId);
  if (!own.length) return null;
  return own.map(wait => {
    const labels = wait.runIds.map(id => {
      const live = runs.find(run => run.runId === id);
      const saved = wait.runs.find(run => run.runId === id);
      const start = Date.parse(live?.startedAt ?? saved?.startedAt ?? "");
      // Missing index evidence is not proof that a historical run is still running.
      // Omit duration for stable announcements and when the live entry is absent.
      const end = now === undefined || !live ? NaN : live.endedAt ? Date.parse(live.endedAt) : now;
      const seconds = Number.isFinite(start) && Number.isFinite(end) ? Math.max(0, Math.floor((end - start) / 1000)) : null;
      const uncertainty = live?.state === "unknown"
        ? `, ${testRunUnknownLabel(live.unknownReason)}${live.unknownReason === "noPid" || live.unknownReason === "heartbeatStale" ? ", wait remains pending" : ""}`
        : "";
      return `${id} (${live?.preset ?? saved?.preset ?? "run"}${live ? `, ${live.state}${uncertainty}${live.currentStage ? `, stage ${live.currentStage}` : ""}` : ", not indexed"}${seconds === null ? "" : `, ${Math.floor(seconds / 60)}m${seconds % 60}s`})`;
    });
    return `Waiting for ${wait.mode === "all" ? "all test runs" : "any test run"}: ${labels.join(", ")}${wait.title ? ` — ${wait.title}` : ""}`;
  }).join("; ");
}
