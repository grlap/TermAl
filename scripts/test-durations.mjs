// Per-test duration report for the shared test launcher.
//
// Owns: the duration budget, the extra runner arguments that make a stage
// write a machine-readable duration artifact into its run directory, reading
// those artifacts back into a bounded report, and the bounded summary lines
// that name the slowest tests at or above the budget and point to the rest. The budget is a repair report: it never
// decides whether a stage passed, never raises a timeout, and an artifact that
// is missing or unreadable is reported as such, never as a pass or a failure.
// Does not own: running stages, stage success, or the launcher summary's
// other sections, which stay in `test-launcher.mjs`; nor the Node event
// encoding, which `node-test-duration-reporter.mjs` writes.
import { readFileSync, realpathSync } from "node:fs";
import { isAbsolute, join, relative, sep } from "node:path";

export const durationBudgetMs = 2000;
// A file is listed by its own, larger threshold: its test time adds up many
// tests, and it is never divided among them.
export const slowFileBudgetMs = 10_000;
export const nodeDurationReporterFile = "scripts/node-test-duration-reporter.mjs";
// The reporter ships beside this module, so a run uses the launcher's own copy
// whatever directory the stage runs in.
export const nodeDurationReporterUrl = new URL("./node-test-duration-reporter.mjs", import.meta.url).href;
const summaryTestLimit = 10;
const summaryFileLimit = 5;
const summaryLineLimit = 220;
// results.json is read by the host under a size cap (1 MiB), so the recorded
// lists are bounded; their totals are exact and the artifact keeps every test.
export const recordedEntryLimit = 200;

export const rustDurationLimitation =
  "stable libtest prints no per-test durations; only the stage time is recorded";

// How each kind of stage reports its test durations.
export const durationReportKinds = Object.freeze([
  "vitest-json",
  "node-test-events",
  "unavailable",
]);

const portable = (path) => path.split(sep).join("/");

// The extra arguments and artifact for one stage, or undefined when the stage
// asks for no report. `unavailable` adds no arguments and names why.
export function durationReportPlan(stage, runDir) {
  switch (stage.durationReport) {
    case undefined:
      return undefined;
    case "vitest-json": {
      const artifact = join(runDir, `${stage.name}-durations.json`);
      return {
        kind: stage.durationReport,
        artifact,
        prefix: [],
        suffix: ["--reporter=default", "--reporter=json", `--outputFile.json=${portable(artifact)}`],
      };
    }
    case "node-test-events": {
      const artifact = join(runDir, `${stage.name}-durations.jsonl`);
      return {
        kind: stage.durationReport,
        artifact,
        // The human log keeps Node's spec output; the events go to the file.
        prefix: [
          "--test-reporter=spec",
          "--test-reporter-destination=stdout",
          `--test-reporter=${nodeDurationReporterUrl}`,
          `--test-reporter-destination=${artifact}`,
        ],
        suffix: [],
      };
    }
    case "unavailable":
      return { kind: stage.durationReport, reason: rustDurationLimitation, prefix: [], suffix: [] };
    default:
      throw new Error(`unknown duration report kind: ${stage.durationReport}`);
  }
}

// The run's base as given and, when the file system resolves it to another
// path, as resolved. A runner started in a directory names its files under
// the resolved path (macOS's /var is a link to /private/var), so a base
// reached through a link must be compared in both forms.
function reportBases(base) {
  if (!base) return [];
  const bases = [base];
  try {
    const resolved = realpathSync.native(base);
    if (resolved !== base) bases.push(resolved);
  } catch {
    // A base that cannot be resolved is compared as given.
  }
  return bases;
}

function displayPath(file, bases) {
  if (typeof file !== "string") return "(unknown file)";
  const native = file.split("/").join(sep);
  if (isAbsolute(native)) {
    for (const base of bases) {
      const local = relative(base, native);
      if (local && !local.startsWith("..") && !isAbsolute(local)) return portable(local);
    }
  }
  return portable(native);
}

// Recorded names and paths are bounded and single-line: results.json is read
// under a byte cap, and each summary entry must stay one line.
const recordedTextLimit = 300;
function recordedText(value) {
  const text = String(value).replace(/[\u0000-\u001f\u007f-\u009f\u2028\u2029]/gu, " ");
  return text.length > recordedTextLimit ? `${text.slice(0, recordedTextLimit - 1)}…` : text;
}

// Vitest's own timeout failure; an assertion that merely mentions a timeout
// is not one.
const vitestTimeout = /^(?:Error: )?Test timed out in \d+ms/mu;

function vitestTests(report, bases) {
  if (!Array.isArray(report?.testResults)) {
    throw new Error("Vitest report has no testResults list");
  }
  const tests = [];
  const files = [];
  for (const fileResult of report.testResults) {
    const file = recordedText(displayPath(fileResult?.name, bases));
    // Vitest's file span: first test start to last test end, hooks between
    // tests included, import and environment excluded.
    if (Number.isFinite(fileResult?.startTime) && Number.isFinite(fileResult?.endTime)) {
      files.push({ file, durationMs: fileResult.endTime - fileResult.startTime });
    }
    for (const assertion of fileResult?.assertionResults ?? []) {
      tests.push({
        file,
        name: recordedText(typeof assertion?.fullName === "string" ? assertion.fullName : assertion?.title),
        state: typeof assertion?.status === "string" ? assertion.status : "unknown",
        // Vitest's per-test duration, which includes the test's own hooks.
        durationMs: Number.isFinite(assertion?.duration) ? assertion.duration : null,
        timedOut: (assertion?.failureMessages ?? []).some((message) => vitestTimeout.test(String(message))),
      });
    }
  }
  return { tests, files };
}

function nodeTests(text, bases) {
  const tests = [];
  const files = [];
  const stacks = new Map();
  for (const raw of text.split("\n")) {
    if (!raw.trim()) continue;
    const event = JSON.parse(raw);
    const file = recordedText(displayPath(event.file, bases));
    if (event.type === "test:summary") {
      if (event.file && Number.isFinite(event.durationMs)) {
        files.push({ file, durationMs: event.durationMs });
      }
      continue;
    }
    if (!Number.isInteger(event.nesting)) continue;
    const stack = stacks.get(file) ?? [];
    stacks.set(file, stack);
    if (event.type === "test:start") {
      stack.length = event.nesting;
      stack[event.nesting] = event.name;
      continue;
    }
    if (event.kind === "suite") continue;
    const ancestors = stack.slice(0, event.nesting).filter((name) => typeof name === "string");
    tests.push({
      file,
      name: recordedText([...ancestors, event.name].join(" > ")),
      state: event.type === "test:fail"
        ? "failed"
        : event.skipped ? "skipped" : event.todo ? "todo" : "passed",
      durationMs: Number.isFinite(event.durationMs) ? event.durationMs : null,
      timedOut: event.timedOut === true,
    });
  }
  return { tests, files };
}

const byDuration = (left, right) => right.durationMs - left.durationMs ||
  left.file.localeCompare(right.file) || (left.name ?? "").localeCompare(right.name ?? "");

// Reads one stage's artifact. Never throws: a missing or malformed artifact is
// a reported state of the duration report, not of the stage.
export function readDurationReport(
  plan,
  base,
  budgetMs = durationBudgetMs,
  fileBudgetMs = slowFileBudgetMs,
) {
  const report = { kind: plan.kind, budgetMs, fileBudgetMs };
  if (plan.kind === "unavailable") {
    return { ...report, status: "unavailable", reason: plan.reason };
  }
  report.artifact = plan.artifact;
  let text;
  try {
    text = readFileSync(plan.artifact, "utf8");
  } catch (error) {
    return { ...report, status: "missing", reason: error.code === "ENOENT" ? "the runner wrote no duration artifact" : error.message };
  }
  try {
    const bases = reportBases(base);
    const { tests, files } = plan.kind === "vitest-json"
      ? vitestTests(JSON.parse(text), bases)
      : nodeTests(text, bases);
    const counted = tests.filter((test) => !["skipped", "pending", "todo"].includes(test.state));
    const overBudget = counted
      .filter((test) => test.durationMs !== null && test.durationMs >= budgetMs)
      .sort(byDuration);
    const slowFiles = files.filter((file) => file.durationMs >= fileBudgetMs).sort(byDuration);
    return {
      ...report,
      status: "measured",
      tests: counted.length,
      missingDuration: counted.filter((test) => test.durationMs === null).length,
      overBudgetCount: overBudget.length,
      overBudget: overBudget.slice(0, recordedEntryLimit),
      slowFileCount: slowFiles.length,
      slowFiles: slowFiles.slice(0, recordedEntryLimit),
    };
  } catch (error) {
    return { ...report, status: "unreadable", reason: error.message };
  }
}

const seconds = (milliseconds) => `${(milliseconds / 1000).toFixed(2)} s`;
const clip = (text) => text.length > summaryLineLimit ? `${text.slice(0, summaryLineLimit - 1)}…` : text;

// Bounded summary lines for every stage that carries a duration report.
export function durationSummaryLines(stages) {
  const reported = stages.filter((stage) => stage.durations);
  if (reported.length === 0) return [];
  const budget = reported[0].durations.budgetMs ?? durationBudgetMs;
  const fileBudget = reported[0].durations.fileBudgetMs ?? slowFileBudgetMs;
  const lines = [
    `durations: a test's time is its runner's figure, which includes its own per-test hooks but not import or environment; a file's time is its runner's file figure (Vitest: first test start to last test end, hooks between tests included); tests at or above ${budget} ms and files at or above ${fileBudget} ms are named for repair; this report never decides a pass`,
  ];
  // The artifact path goes on its own unclipped line, so it is never cut.
  const artifactLine = (report) => report.artifact ? [`  report ${report.artifact}`] : [];
  for (const stage of reported) {
    const report = stage.durations;
    if (report.status !== "measured") {
      lines.push(clip(`${stage.name}: per-test durations ${report.status}: ${String(report.reason).replace(/\s+/gu, " ")}`));
      lines.push(...artifactLine(report));
      continue;
    }
    const missing = report.missingDuration ? `; ${report.missingDuration} without a duration` : "";
    const testCount = report.overBudgetCount ?? report.overBudget.length;
    const fileCount = report.slowFileCount ?? report.slowFiles.length;
    lines.push(clip(`${stage.name}: ${testCount} of ${report.tests} tests >= ${report.budgetMs} ms; ${fileCount} files >= ${report.fileBudgetMs ?? slowFileBudgetMs} ms${missing}`));
    lines.push(...artifactLine(report));
    const shownTests = report.overBudget.slice(0, summaryTestLimit);
    for (const test of shownTests) {
      const state = test.timedOut ? " [timed out]" : test.state === "passed" ? "" : ` [${test.state}]`;
      lines.push(clip(`  ${seconds(test.durationMs)} ${test.file}: ${test.name}${state}`));
    }
    if (testCount > shownTests.length) {
      lines.push(`  and ${testCount - shownTests.length} more tests in the report`);
    }
    const shownFiles = report.slowFiles.slice(0, summaryFileLimit);
    for (const file of shownFiles) {
      lines.push(clip(`  file ${seconds(file.durationMs)} ${file.file}`));
    }
    if (fileCount > shownFiles.length) {
      lines.push(`  and ${fileCount - shownFiles.length} more files in the report`);
    }
  }
  return lines;
}
