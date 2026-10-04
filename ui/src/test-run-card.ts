// Transcript-owned test-run snapshots and display text. No live-index joins,
// process inference, launcher execution, or notification ownership.
import type { TestRunStageSummary, TestRunState, TestRunUnknownReason } from "./test-runs";

export interface TestRunCardSnapshot {
  runId: string;
  worktree: string;
  runDir: string;
  preset: "full" | "focused" | "live";
  detached: boolean | null;
  state: TestRunState;
  unknownReason?: TestRunUnknownReason | "notIndexed" | null;
  interrupted: boolean;
  currentStage: string | null;
  startedAt: string | null;
  endedAt: string | null;
  exitCode: number | null;
  stages: TestRunStageSummary[];
  stagesOmitted: number;
  error: string | null;
  errorTruncated: boolean;
  failure: { phase: "stage" | "preflight"; name: string; excerpt: string; truncated: boolean } | null;
  command: string[] | null;
  commandTruncated: boolean;
}

export interface TestRunCardTarget { runId: string; runDir: string }

export function testRunInvocation(run: TestRunCardSnapshot) {
  return `node scripts/test-launcher.mjs ${run.preset}${run.preset === "focused" && run.command?.length ? ` -- ${run.command.join(" ")}` : ""}${run.commandTruncated ? " … (truncated)" : ""}`;
}

export function testRunStatusLine(run: TestRunCardSnapshot) {
  return `${run.runId}${run.interrupted ? " · interrupted" : ""}${run.exitCode !== null ? ` · exit ${run.exitCode}` : ""}`;
}

export function testRunUnknownText(run: TestRunCardSnapshot) {
  return `${testRunUnknownLabel(run.unknownReason)}.${run.unknownReason === "processGone"
    ? " Use the launcher's recover command to inspect whether this run can be settled."
    : " This is not a passing result."}`;
}

export function testRunStageText(run: TestRunCardSnapshot, stage: TestRunStageSummary) {
  return `${stage.name}: ${stage.state === "running" && (run.state === "unknown" || run.interrupted) ? "interrupted" : stage.state}`;
}

export function testRunSearchText(run: TestRunCardSnapshot) {
  return ["Test run", run.state, testRunInvocation(run), testRunStatusLine(run),
    run.state === "unknown" ? testRunUnknownText(run) : "",
    run.currentStage ? `Stage: ${run.currentStage}` : "", ...run.stages.map(stage => testRunStageText(run, stage)),
    run.stagesOmitted > 0 ? `${run.stagesOmitted} stages omitted from this card. See Details.` : "",
    run.error ? `${run.error}${run.errorTruncated ? " … (truncated)" : ""}` : "",
    run.state === "failed" && run.failure ? `${run.failure.phase}: ${run.failure.name}\n${run.failure.excerpt}${run.failure.truncated ? "\nExcerpt truncated; full log is available in Details." : ""}` : "",
  ].filter(Boolean).join("\n");
}

export function testRunUnknownLabel(reason: TestRunCardSnapshot["unknownReason"]) {
  switch (reason) {
    case "processGone": return "responsible process is gone";
    case "noPid": return "executor not published; liveness uncertain";
    case "heartbeatStale": return "heartbeat stale; executor not published, liveness uncertain";
    case "resultsUnreadable": return "results unreadable";
    case "notIndexed": return "run no longer indexed";
    default: return "terminal evidence is unknown";
  }
}
