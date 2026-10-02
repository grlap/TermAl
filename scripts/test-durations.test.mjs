// Exercises the launcher's per-test duration report without running product gates.
// Every threshold decision here uses recorded durations or a zero budget, never
// wall-clock timing, so the outcome cannot depend on machine load.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import {
  durationBudgetMs,
  durationReportPlan,
  durationSummaryLines,
  nodeDurationReporterFile,
  readDurationReport,
  recordedEntryLimit,
  rustDurationLimitation,
  slowFileBudgetMs,
} from "./test-durations.mjs";
import { testTempDirectory } from "./test-temp-root.mjs";

const projectRoot = fileURLToPath(new URL("../", import.meta.url));

// A runner started from inside this test file would otherwise inherit the
// parent runner's child-protocol marker and stream events to it instead of
// writing its own reporters' output.
function standaloneRunnerEnv(env = process.env) {
  const { NODE_TEST_CONTEXT: _parentRunner, ...rest } = env;
  return rest;
}

function scratch(t) {
  const directory = mkdtempSync(join(testTempDirectory(), "durations-fixture-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return directory;
}

test("the budgets are two seconds per test and ten per file, and a report plan only adds runner output arguments", () => {
  assert.equal(durationBudgetMs, 2000);
  assert.equal(slowFileBudgetMs, 10_000);
  const runDir = join("run", "test-x");
  assert.equal(durationReportPlan({ name: "plain" }, runDir), undefined);

  const vitest = durationReportPlan({ name: "vitest", durationReport: "vitest-json" }, runDir);
  assert.equal(vitest.artifact, join(runDir, "vitest-durations.json"));
  assert.deepEqual(vitest.prefix, []);
  assert.deepEqual(vitest.suffix.slice(0, 2), ["--reporter=default", "--reporter=json"]);
  assert.match(vitest.suffix[2], /^--outputFile\.json=run\/test-x\/vitest-durations\.json$/u);

  const node = durationReportPlan({ name: "helpers", durationReport: "node-test-events" }, runDir);
  assert.equal(node.artifact, join(runDir, "helpers-durations.jsonl"));
  assert.deepEqual(node.prefix.slice(0, 2), ["--test-reporter=spec", "--test-reporter-destination=stdout"]);
  assert.equal(node.prefix[2], `--test-reporter=${pathToFileURL(join(projectRoot, nodeDurationReporterFile)).href}`);
  assert.equal(node.prefix[3], `--test-reporter-destination=${node.artifact}`);
  assert.deepEqual(node.suffix, []);

  const rust = durationReportPlan({ name: "rust-tests", durationReport: "unavailable" }, runDir);
  assert.deepEqual([rust.prefix, rust.suffix, rust.reason], [[], [], rustDurationLimitation]);
  assert.throws(() => durationReportPlan({ name: "x", durationReport: "guess" }, runDir), /unknown duration report kind/u);
});

test("a Vitest report names tests at or above the budget and keeps failed, timed-out and missing durations distinct", (t) => {
  const directory = scratch(t);
  const base = join(directory, "ui");
  const artifact = join(directory, "vitest-durations.json");
  const file = (name) => join(base, "src", name).split("\\").join("/");
  writeFileSync(artifact, JSON.stringify({
    testResults: [
      {
        name: file("Slow.test.tsx"),
        startTime: 1000,
        endTime: 11000,
        assertionResults: [
          { fullName: "Slow renders a tall page", status: "passed", duration: 2500, failureMessages: [] },
          { fullName: "Slow is under the budget", status: "passed", duration: 1999.9, failureMessages: [] },
          { fullName: "Slow waits forever", status: "failed", duration: 3000, failureMessages: ["Error: Test timed out in 10000ms."] },
          { fullName: "Slow asserts badly", status: "failed", duration: 2000, failureMessages: ["AssertionError: expected 1 to be 2"] },
          { fullName: "Slow mentions a timeout", status: "failed", duration: 2100, failureMessages: ["AssertionError: expected the request to have timed out"] },
          { fullName: "Slow is skipped", status: "skipped", failureMessages: [] },
          { fullName: "Slow lost its duration", status: "passed", failureMessages: [] },
        ],
      },
      { name: file("Fast.test.ts"), startTime: 0, endTime: 5, assertionResults: [] },
      // Over the test budget but under the file threshold: not a slow file.
      { name: file("Medium.test.ts"), startTime: 0, endTime: 9999, assertionResults: [] },
    ],
  }));
  const report = readDurationReport({ kind: "vitest-json", artifact }, base);
  assert.equal(report.status, "measured");
  assert.equal(report.tests, 6, "a skipped test is not counted");
  assert.equal(report.missingDuration, 1);
  assert.deepEqual(report.overBudget.map(({ name, state, timedOut, durationMs }) => [name, state, timedOut, durationMs]), [
    ["Slow waits forever", "failed", true, 3000],
    ["Slow renders a tall page", "passed", false, 2500],
    ["Slow mentions a timeout", "failed", false, 2100],
    ["Slow asserts badly", "failed", false, 2000],
  ]);
  assert.equal(report.overBudget[0].file, "src/Slow.test.tsx");
  assert.deepEqual(report.slowFiles, [{ file: "src/Slow.test.tsx", durationMs: 10_000 }]);
  assert.equal(report.fileBudgetMs, 10_000);
});

test("Node runner events give nested test names and per-file time, and suites are not tests", (t) => {
  const directory = scratch(t);
  const file = join(directory, "helper.test.mjs");
  const artifact = join(directory, "helpers-durations.jsonl");
  const event = (type, fields) => JSON.stringify({ type, file, kind: null, durationMs: null, skipped: false, todo: false, timedOut: false, ...fields });
  writeFileSync(artifact, [
    event("test:start", { name: "suite", nesting: 0 }),
    event("test:start", { name: "inner", nesting: 1 }),
    event("test:pass", { name: "inner", nesting: 1, kind: "test", durationMs: 2100 }),
    event("test:pass", { name: "suite", nesting: 0, kind: "suite", durationMs: 2200 }),
    event("test:start", { name: "hangs", nesting: 0 }),
    event("test:fail", { name: "hangs", nesting: 0, kind: "test", durationMs: 5000, timedOut: true }),
    event("test:start", { name: "skipped", nesting: 0 }),
    event("test:pass", { name: "skipped", nesting: 0, kind: "test", durationMs: 0.1, skipped: true }),
    event("test:start", { name: "quick", nesting: 0 }),
    event("test:pass", { name: "quick", nesting: 0, kind: "test", durationMs: 3 }),
    event("test:summary", { name: null, nesting: null, durationMs: 12_400 }),
    event("test:summary", { name: null, nesting: null, file: null, durationMs: 12_500 }),
    "",
  ].join("\n"));
  const report = readDurationReport({ kind: "node-test-events", artifact }, directory);
  assert.equal(report.status, "measured");
  assert.equal(report.tests, 3);
  assert.deepEqual(report.overBudget.map(({ name, state, timedOut, file: path }) => [name, state, timedOut, path]), [
    ["hangs", "failed", true, "helper.test.mjs"],
    ["suite > inner", "passed", false, "helper.test.mjs"],
  ]);
  assert.deepEqual(report.slowFiles, [{ file: "helper.test.mjs", durationMs: 12_400 }]);
});

test("a run reached through a directory link names files relative to it, as runners name them resolved", (t) => {
  // macOS's temporary directories are under /var, a link to /private/var: a
  // runner started there names its files under the resolved path.
  const directory = scratch(t);
  const real = join(directory, "real");
  const link = join(directory, "link");
  mkdirSync(real);
  symlinkSync(real, link, "junction");
  const resolved = realpathSync.native(real);
  const nodeArtifact = join(real, "helpers-durations.jsonl");
  writeFileSync(nodeArtifact, [
    JSON.stringify({ type: "test:start", file: join(resolved, "linked.test.mjs"), name: "linked", nesting: 0 }),
    JSON.stringify({
      type: "test:pass", file: join(resolved, "linked.test.mjs"), name: "linked", nesting: 0,
      kind: "test", durationMs: 2500, skipped: false, todo: false, timedOut: false,
    }),
    "",
  ].join("\n"));
  const node = readDurationReport({ kind: "node-test-events", artifact: nodeArtifact }, link);
  assert.deepEqual(node.overBudget.map(({ file }) => file), ["linked.test.mjs"]);

  const vitestArtifact = join(real, "vitest-durations.json");
  writeFileSync(vitestArtifact, JSON.stringify({
    testResults: [{
      name: join(resolved, "src", "Linked.test.tsx"),
      startTime: 0,
      endTime: 11_000,
      assertionResults: [{ fullName: "linked", status: "passed", duration: 2500, failureMessages: [] }],
    }],
  }));
  const vitest = readDurationReport({ kind: "vitest-json", artifact: vitestArtifact }, link);
  assert.deepEqual(vitest.overBudget.map(({ file }) => file), ["src/Linked.test.tsx"]);
  assert.deepEqual(vitest.slowFiles, [{ file: "src/Linked.test.tsx", durationMs: 11_000 }]);
});

test("the recorded lists are bounded for the host's results cap while totals stay exact", (t) => {
  const directory = scratch(t);
  const artifact = join(directory, "vitest-durations.json");
  writeFileSync(artifact, JSON.stringify({
    testResults: Array.from({ length: 250 }, (_, index) => ({
      name: join(directory, `F${index}.test.ts`),
      startTime: 0,
      endTime: 12_000,
      assertionResults: [{ fullName: `case ${index}`, status: "passed", duration: 3000 + index, failureMessages: [] }],
    })),
  }));
  const report = readDurationReport({ kind: "vitest-json", artifact }, directory);
  assert.equal(recordedEntryLimit, 200);
  assert.deepEqual(
    [report.tests, report.overBudgetCount, report.overBudget.length, report.slowFileCount, report.slowFiles.length],
    [250, 250, 200, 250, 200],
  );
  assert.equal(report.overBudget[0].name, "case 249", "the slowest tests are the ones kept");
  const lines = durationSummaryLines([{ name: "vitest", durations: report }]);
  assert.match(lines[1], /^vitest: 250 of 250 tests >= 2000 ms; 250 files >= 10000 ms/u);
  assert.ok(lines.includes("  and 240 more tests in the report"));
  assert.ok(lines.includes("  and 245 more files in the report"));
});

test("recorded names and paths are single-line and bounded", (t) => {
  const directory = scratch(t);
  const artifact = join(directory, "vitest-durations.json");
  writeFileSync(artifact, JSON.stringify({
    testResults: [{
      name: join(directory, `${"deep/".repeat(80)}Long.test.ts`),
      startTime: 0,
      endTime: 1,
      assertionResults: [{
        fullName: `each case\nwith a newline\u2028and ${"x".repeat(5000)}`,
        status: "passed",
        duration: 2500,
        failureMessages: [],
      }],
    }],
  }));
  const [test] = readDurationReport({ kind: "vitest-json", artifact }, directory).overBudget;
  assert.equal(test.name.length, 300);
  assert.ok(test.name.endsWith("…"));
  assert.doesNotMatch(test.name, /[\n\u2028]/u);
  assert.ok(test.file.length <= 300);
  const lines = durationSummaryLines([{ name: "vitest", durations: readDurationReport({ kind: "vitest-json", artifact }, directory) }]);
  const entryLines = lines.filter((line) => line.startsWith("  2.50 s "));
  assert.equal(entryLines.length, 1, "one summary line per entry");
  assert.ok(lines.every((line) => !/[\n\u2028]/u.test(line)), "no summary line carries a line break");
});

test("a missing or malformed artifact is a reported state and never throws", (t) => {
  const directory = scratch(t);
  const missing = readDurationReport({ kind: "vitest-json", artifact: join(directory, "absent.json") }, directory);
  assert.equal(missing.status, "missing");
  assert.match(missing.reason, /wrote no duration artifact/u);

  const broken = join(directory, "broken.json");
  writeFileSync(broken, "{not json");
  assert.equal(readDurationReport({ kind: "vitest-json", artifact: broken }, directory).status, "unreadable");
  writeFileSync(broken, JSON.stringify({ results: [] }));
  assert.match(readDurationReport({ kind: "vitest-json", artifact: broken }, directory).reason, /no testResults/u);
  writeFileSync(broken, "{\"type\":\"test:pass\"\n");
  assert.equal(readDurationReport({ kind: "node-test-events", artifact: broken }, directory).status, "unreadable");

  const rust = readDurationReport({ kind: "unavailable", reason: rustDurationLimitation }, directory);
  assert.deepEqual([rust.status, rust.reason], ["unavailable", rustDurationLimitation]);
});

test("the summary names every stage's slow tests within a bound and states what it is not", () => {
  const slow = Array.from({ length: 13 }, (_, index) => ({
    file: "src/Big.test.tsx",
    name: `case ${index}`,
    state: index === 0 ? "failed" : "passed",
    timedOut: index === 1,
    durationMs: 9000 - index * 100,
  }));
  const files = Array.from({ length: 7 }, (_, index) => ({ file: `src/F${index}.test.tsx`, durationMs: 14_000 - index }));
  const lines = durationSummaryLines([
    { name: "cargo-check" },
    {
      name: "vitest",
      durations: {
        kind: "vitest-json",
        budgetMs: 2000,
        fileBudgetMs: 10_000,
        artifact: "run/vitest-durations.json",
        status: "measured",
        tests: 4400,
        missingDuration: 2,
        overBudget: slow,
        slowFiles: files,
      },
    },
    { name: "rust-tests", durations: { kind: "unavailable", budgetMs: 2000, status: "unavailable", reason: rustDurationLimitation } },
    { name: "fingerprint-tests", durations: { kind: "node-test-events", budgetMs: 2000, artifact: "run/h.jsonl", status: "missing", reason: "the runner wrote no duration artifact" } },
  ]);
  assert.equal(
    lines[0],
    "durations: a test's time is its runner's figure, which includes its own per-test hooks but not import or environment; a file's time is its runner's file figure (Vitest: first test start to last test end, hooks between tests included); tests at or above 2000 ms and files at or above 10000 ms are named for repair; this report never decides a pass",
  );
  assert.equal(lines[1], "vitest: 13 of 4400 tests >= 2000 ms; 7 files >= 10000 ms; 2 without a duration");
  assert.equal(lines[2], "  report run/vitest-durations.json", "the artifact path has its own unclipped line");
  assert.equal(lines[3], "  9.00 s src/Big.test.tsx: case 0 [failed]");
  assert.equal(lines[4], "  8.90 s src/Big.test.tsx: case 1 [timed out]");
  assert.equal(lines.filter((line) => /src\/Big\.test\.tsx: case/u.test(line)).length, 10);
  assert.ok(lines.includes("  and 3 more tests in the report"));
  assert.equal(lines.filter((line) => line.startsWith("  file ")).length, 5);
  assert.ok(lines.includes("  and 2 more files in the report"));
  assert.ok(lines.some((line) => line === `rust-tests: per-test durations unavailable: ${rustDurationLimitation}`));
  assert.ok(lines.some((line) => line === "fingerprint-tests: per-test durations missing: the runner wrote no duration artifact"));
  assert.ok(lines.includes("  report run/h.jsonl"));
  const deep = `run/${"very-deep-directory/".repeat(30)}vitest-durations.json`;
  const deepLines = durationSummaryLines([{ name: "vitest", durations: { kind: "vitest-json", budgetMs: 2000, fileBudgetMs: 10_000, artifact: deep, status: "measured", tests: 1, missingDuration: 0, overBudget: [], slowFiles: [] } }]);
  assert.ok(deepLines.includes(`  report ${deep}`), "a long artifact path is never cut");
  assert.deepEqual(durationSummaryLines([{ name: "focused" }]), []);
});

// The summary says a test's time includes its own per-test hooks. A 300 ms
// beforeEach before an empty body sets a lower bound that machine load can
// only raise, so this cannot pass or fail with load.
test("a test's runner duration includes its own per-test hooks", (t) => {
  const directory = scratch(t);
  const fixture = join(directory, "hook.test.mjs");
  writeFileSync(fixture, [
    "import test, { beforeEach, describe } from 'node:test';",
    "describe('group', () => {",
    "  beforeEach(() => new Promise((resolve) => setTimeout(resolve, 300)));",
    "  test('empty body', () => {});",
    "});",
    "",
  ].join("\n"));
  const plan = durationReportPlan({ name: "helpers", durationReport: "node-test-events" }, directory);
  const run = spawnSync(process.execPath, [...plan.prefix, "--test", fixture], {
    cwd: directory,
    env: standaloneRunnerEnv(),
    encoding: "utf8",
    windowsHide: true,
  });
  assert.equal(run.status, 0, `${run.stdout}\n${run.stderr}`);
  const [test] = readDurationReport(plan, directory, 0).overBudget;
  assert.equal(test.name, "group > empty body");
  assert.ok(test.durationMs >= 250, `the hook's wait is in the test's time: ${test.durationMs} ms`);
});

test("the Node reporter records real runner events that the report reads back", (t) => {
  const directory = scratch(t);
  const fixture = join(directory, "fixture.test.mjs");
  writeFileSync(fixture, [
    "import test, { describe } from 'node:test';",
    "describe('group', () => { test('nested', () => {}); });",
    "test('top', () => {});",
    "test('skipped', { skip: true }, () => {});",
    "",
  ].join("\n"));
  const plan = durationReportPlan({ name: "helpers", durationReport: "node-test-events" }, directory);
  const run = spawnSync(process.execPath, [...plan.prefix, "--test", fixture], {
    cwd: directory,
    env: standaloneRunnerEnv(),
    encoding: "utf8",
    windowsHide: true,
  });
  assert.equal(run.status, 0, `${run.stdout}\n${run.stderr}`);
  assert.match(run.stdout, /✔ top/u, "the human log keeps the spec reporter");
  // A zero budget names every measured test, so nothing depends on how fast
  // this machine runs the fixture.
  const report = readDurationReport(plan, directory, 0);
  assert.equal(report.status, "measured");
  assert.equal(report.missingDuration, 0);
  assert.deepEqual(report.overBudget.map(({ name }) => name).sort(), ["group > nested", "top"]);
  assert.ok(report.overBudget.every(({ file, state }) => file === "fixture.test.mjs" && state === "passed"));
});
