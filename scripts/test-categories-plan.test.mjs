// Exercises the UI category planner without running Vitest: category names
// from the real manifest, and the per-project accounting of a run's report.
import assert from "node:assert/strict";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  accountingSummaryLines,
  accountVitestRun,
  categoryNames,
  listUiTestFiles,
  loadCategoryManifest,
  partialCheckLabel,
  resolveCategory,
  serializedLaneProject,
} from "./test-categories-plan.mjs";

const projectRoot = fileURLToPath(new URL("../", import.meta.url));
const uiRoot = join(projectRoot, "ui");

// A small manifest in the shape of ui/test-categories.ts.
function manifestOf(projects) {
  return {
    projects: projects.map(([name, files, groupOrder]) => ({
      name, include: files, exclude: [], groupOrder, environment: "jsdom",
    })),
    projectSelects: (project, file) => project.include.includes(file),
  };
}
const sample = manifestOf([
  ["unit", ["src/a.test.ts", "src/b.test.ts"], 1],
  ["heavy", ["src/c.test.tsx"], 2],
]);
const sampleBaseline = ["src/a.test.ts", "src/b.test.ts", "src/c.test.tsx"];

// A Vitest JSON report: one entry per file, in run order, one at a time.
function reportOf(files, { failed = [], skipped = [], overlap = false } = {}) {
  let clock = 1000;
  return JSON.stringify({
    testResults: files.map((file) => {
      const startTime = overlap ? 1000 : clock;
      clock += 10;
      return {
        name: join(uiRoot, file).split("\\").join("/"),
        status: failed.includes(file) ? "failed" : "passed",
        startTime,
        endTime: startTime + 10,
        assertionResults: [{
          fullName: `${file} works`,
          status: failed.includes(file) ? "failed" : skipped.includes(file) ? "skipped" : "passed",
        }],
      };
    }),
  });
}

const account = (overrides) => accountVitestRun({
  manifest: sample,
  uiRoot,
  baseline: sampleBaseline,
  accounted: ["unit", "heavy"],
  ...overrides,
});

test("the categories are the manifest's projects, named ui-<project>, and nothing else", async () => {
  const manifest = await loadCategoryManifest(projectRoot);
  assert.deepEqual(categoryNames(manifest), ["ui-unit", "ui-component", "ui-heavy", "ui-app"]);
  for (const project of manifest.projects) {
    assert.equal(resolveCategory(manifest, `ui-${project.name}`), project);
  }
  for (const name of ["ui-bogus", "heavy", "ui-", "UI-HEAVY"]) {
    assert.throws(() => resolveCategory(manifest, name), /unknown category: .*known categories: ui-unit, ui-component, ui-heavy, ui-app/u);
  }
  assert.equal(partialCheckLabel, "partial check, not the full gate");
});

test("every active UI test file belongs to exactly one project, so the full run covers it once", async () => {
  const manifest = await loadCategoryManifest(projectRoot);
  const baseline = listUiTestFiles(uiRoot);
  assert.ok(baseline.length > 100, `baseline listed only ${baseline.length} files`);
  // Vitest runs the projects one group after another.
  const groupOf = (file) => manifest.projects
    .find((project) => manifest.projectSelects(project, file))?.groupOrder ?? Infinity;
  const runOrder = [...baseline].sort((left, right) => groupOf(left) - groupOf(right));
  const accounting = accountVitestRun({
    manifest,
    uiRoot,
    baseline,
    accounted: manifest.projects.map((project) => project.name),
    reportText: reportOf(runOrder),
  });
  assert.deepEqual(accounting.problems, []);
  assert.equal(accounting.status, "complete");
  const total = accounting.projects.reduce((sum, row) => sum + row.selectedFiles, 0);
  assert.equal(total, baseline.length, "the projects' selections partition the baseline");
  for (const row of accounting.projects) assert.ok(row.selectedFiles > 0, `${row.category} selects nothing`);
});

test("the serialized lane is the heavy project, in a group of its own", async () => {
  const manifest = await loadCategoryManifest(projectRoot);
  assert.equal(serializedLaneProject, "heavy");
  const orders = manifest.projects.map((project) => project.groupOrder);
  assert.equal(new Set(orders).size, orders.length, "each project has its own group");
  const accounting = account({ reportText: reportOf(sampleBaseline) });
  assert.deepEqual(accounting.projects.map((row) => [row.category, row.lane]), [
    ["ui-unit", "ordered"],
    ["ui-heavy", "serialized"],
  ]);
});

test("a complete run gives one passed child row per project, from the run's own report", () => {
  const accounting = account({ reportText: reportOf(sampleBaseline, { skipped: ["src/b.test.ts"] }) });
  assert.equal(accounting.status, "complete");
  assert.deepEqual(accounting.projects.map((row) => ({
    kind: row.kind, category: row.category, status: row.status,
    selected: row.selectedFiles, executed: row.executedFiles, tests: row.tests,
  })), [
    { kind: "test", category: "ui-unit", status: "passed", selected: 2, executed: 2,
      tests: { total: 2, passed: 1, failed: 0, skipped: 1, pending: 0 } },
    { kind: "test", category: "ui-heavy", status: "passed", selected: 1, executed: 1,
      tests: { total: 1, passed: 1, failed: 0, skipped: 0, pending: 0 } },
  ]);
});

// The sample report's file results, to damage one field at a time.
const sampleResults = () => JSON.parse(reportOf(sampleBaseline)).testResults;
const accountResults = (testResults) => account({ reportText: JSON.stringify({ testResults }) });

test("a file result Vitest would not write is unknown, never a pass", () => {
  for (const [label, damage] of [
    ["no file status", (result) => { delete result.status; }],
    ["a file status other than passed or failed", (result) => { result.status = "skipped"; }],
    ["no assertionResults list", (result) => { delete result.assertionResults; }],
    ["a test without a status", (result) => { result.assertionResults = [{ fullName: "a" }]; }],
    ["a test with an unknown status", (result) => { result.assertionResults = [{ status: "done" }]; }],
    ["no start time", (result) => { delete result.startTime; }],
    ["no end time", (result) => { delete result.endTime; }],
    ["an end before its start", (result) => { result.endTime = result.startTime - 1; }],
  ]) {
    const results = sampleResults();
    damage(results[0]);
    const accounting = accountResults(results);
    assert.equal(accounting.status, "incomplete", label);
    const unit = accounting.projects.find((row) => row.category === "ui-unit");
    assert.equal(unit.status, "unknown", label);
    assert.equal(unit.malformedFiles, 1, label);
    assert.match(unit.malformed[0], /^src\/a\.test\.ts: /u, label);
    assert.match(accounting.reason, /ui-unit unknown/u, label);
  }
  // Every selected file named, and nothing else: no outcome was observed.
  const namesOnly = accountResults(sampleResults().map(({ name }) => ({ name })));
  assert.equal(namesOnly.status, "incomplete");
  assert.deepEqual(namesOnly.projects.map((row) => row.status), ["unknown", "unknown"]);
  assert.deepEqual(namesOnly.projects.map((row) => row.tests.passed), [0, 0]);
});

test("a test Vitest did not finish leaves its project incomplete; skips and todos do not", () => {
  const pending = sampleResults();
  pending[2].assertionResults = [{ fullName: "c", status: "pending" }];
  const unfinished = accountResults(pending);
  assert.equal(unfinished.status, "incomplete");
  const heavy = unfinished.projects.find((row) => row.category === "ui-heavy");
  assert.equal(heavy.status, "incomplete");
  assert.equal(heavy.tests.pending, 1);
  assert.match(accountingSummaryLines([{ name: "vitest", accounting: unfinished }]).join("\n"),
    /ui-heavy \[serialized lane\]: incomplete: .*1 not finished/u);

  const intentional = sampleResults();
  intentional[0].assertionResults.push({ fullName: "a later", status: "todo" });
  intentional[1].assertionResults = [{ fullName: "b", status: "skipped" }];
  const skipped = accountResults(intentional);
  assert.equal(skipped.status, "complete");
  assert.deepEqual(skipped.projects[0].tests, { total: 3, passed: 1, failed: 0, skipped: 2, pending: 0 });
});

test("a file whose tests were all skipped says nothing about order or overlap", () => {
  // Vitest gives such a file its run start as both start and end time.
  const results = sampleResults();
  const runStart = results[0].startTime - 100;
  results[2].assertionResults = [{ fullName: "c", status: "skipped" }];
  results[2].startTime = runStart;
  results[2].endTime = runStart;
  const accounting = accountResults(results);
  assert.deepEqual(accounting.problems, []);
  assert.equal(accounting.status, "complete");
  const heavy = accounting.projects.find((row) => row.category === "ui-heavy");
  assert.equal(heavy.status, "passed");
  assert.equal(heavy.tests.skipped, 1);
  assert.equal("firstStart" in heavy, false);
  // A heavy file that did run that early still breaks the declared order.
  results[2].assertionResults = [{ fullName: "c", status: "passed" }];
  results[2].endTime = runStart + 5000;
  assert.match(accountResults(results).problems.join("\n"), /ui-heavy started before ui-unit ended/u);
});

test("a failed, unrun, empty or unserialized project leaves the accounting incomplete", () => {
  for (const [label, overrides, category, status] of [
    ["a failing test", { reportText: reportOf(sampleBaseline, { failed: ["src/c.test.tsx"] }) }, "ui-heavy", "failed"],
    ["a selected file that never ran", { reportText: reportOf(["src/a.test.ts", "src/c.test.tsx"]) }, "ui-unit", "incomplete"],
    ["a project that ran nothing", { reportText: reportOf(["src/a.test.ts", "src/b.test.ts"]) }, "ui-heavy", "empty"],
    ["files that ran at once", { reportText: reportOf(sampleBaseline, { overlap: true }) }, "ui-unit", "unserialized"],
  ]) {
    const accounting = account(overrides);
    assert.equal(accounting.status, "incomplete", label);
    assert.equal(accounting.projects.find((row) => row.category === category).status, status, label);
    assert.match(accounting.reason, new RegExp(`${category} ${status}`, "u"), label);
  }
  const unrun = account({ reportText: reportOf(["src/a.test.ts", "src/c.test.tsx"]) });
  assert.deepEqual(unrun.projects[0].unrun, ["src/b.test.ts"]);
});

test("a missing or unreadable report is unknown, never a pass", () => {
  for (const [label, overrides] of [
    ["missing", { reportError: "the runner wrote no Vitest report" }],
    ["malformed", { reportText: "{not json" }],
    ["wrong shape", { reportText: JSON.stringify({ results: [] }) }],
  ]) {
    const accounting = account(overrides);
    assert.equal(accounting.status, "unknown", label);
    assert.ok(accounting.projects.every((row) => row.status === "unknown"), label);
    assert.ok(accounting.projects.every((row) => !("tests" in row)), `${label}: no invented counts`);
  }
});

test("a file outside the plan, a file in two projects, or groups out of order are problems", () => {
  const stray = account({ reportText: reportOf([...sampleBaseline, "src/stray.test.ts"]) });
  assert.equal(stray.status, "incomplete");
  assert.match(stray.problems.join("\n"), /src\/stray\.test\.ts ran but no accounted project selects it/u);

  const overlapping = manifestOf([
    ["unit", ["src/a.test.ts"], 1],
    ["heavy", ["src/a.test.ts"], 2],
  ]);
  const twice = accountVitestRun({
    manifest: overlapping, uiRoot, baseline: ["src/a.test.ts"], accounted: ["unit", "heavy"],
    reportText: reportOf(["src/a.test.ts"]),
  });
  assert.match(twice.problems.join("\n"), /src\/a\.test\.ts is selected by 2 projects/u);

  // The heavy file runs before the unit files, against the declared order.
  const reversed = account({ reportText: reportOf(["src/c.test.tsx", "src/a.test.ts", "src/b.test.ts"]) });
  assert.equal(reversed.status, "incomplete");
  assert.match(reversed.problems.join("\n"), /ui-heavy started before ui-unit ended/u);
});

// Three projects, one file each; `times` gives each file's start, or null for
// a file whose tests were all skipped (Vitest stamps it with the run start).
function accountThree(groups, times) {
  const files = ["src/a.test.ts", "src/b.test.tsx", "src/c.test.tsx"];
  const manifest = manifestOf([
    ["unit", [files[0]], groups[0]],
    ["component", [files[1]], groups[1]],
    ["heavy", [files[2]], groups[2]],
  ]);
  const runStart = 10;
  return accountVitestRun({
    manifest,
    uiRoot,
    baseline: files,
    accounted: ["unit", "component", "heavy"],
    reportText: JSON.stringify({
      testResults: files.map((file, index) => {
        const skipped = times[index] === null;
        const startTime = skipped ? runStart : times[index];
        return {
          name: join(uiRoot, file).split("\\").join("/"),
          status: "passed",
          startTime,
          endTime: skipped ? startTime : startTime + 10,
          assertionResults: [{ fullName: file, status: skipped ? "skipped" : "passed" }],
        };
      }),
    }),
  });
}

test("an all-skipped group cannot hide an order violation between the groups around it", () => {
  const outOfOrder = accountThree([1, 2, 3], [200, null, 100]);
  assert.match(outOfOrder.problems.join("\n"), /ui-heavy started before ui-unit ended/u);
  assert.equal(outOfOrder.status, "incomplete");
  const inOrder = accountThree([1, 2, 3], [100, null, 200]);
  assert.deepEqual(inOrder.problems, []);
  assert.equal(inOrder.status, "complete");
  // A group may start exactly when the latest earlier one ended.
  assert.equal(accountThree([1, 2, 3], [100, 110, 120]).status, "complete");
});

test("a project without a test window keeps its coverage status; the window is timing only", () => {
  // All skipped and whole: passed, with no window.
  const skippedOnly = sampleResults();
  skippedOnly[2].assertionResults = [{ fullName: "c", status: "skipped" }];
  const heavyOf = (results) => accountResults(results).projects
    .find((row) => row.category === "ui-heavy");
  assert.equal(heavyOf(skippedOnly).status, "passed");
  assert.equal("firstStart" in heavyOf(skippedOnly), false);
  // No window, and a test not finished: still incomplete.
  const pending = sampleResults();
  pending[2].assertionResults = [
    { fullName: "c", status: "skipped" },
    { fullName: "c later", status: "pending" },
  ];
  assert.equal(heavyOf(pending).status, "incomplete");
  // No window, and a malformed file result: still unknown.
  const malformed = sampleResults();
  malformed[2].assertionResults = [{ fullName: "c", status: "skipped" }];
  delete malformed[2].startTime;
  assert.equal(heavyOf(malformed).status, "unknown");
  // No window because nothing ran: still empty, never passed.
  assert.equal(heavyOf(sampleResults().slice(0, 2)).status, "empty");
});

test("projects sharing a group are checked together against the earlier groups only", () => {
  // component and heavy share group 2: their overlap is no order violation.
  const together = accountThree([1, 2, 2], [100, 120, 125]);
  assert.deepEqual(together.problems, []);
  assert.equal(together.status, "complete");
  // Both started before the earlier group ended: each is reported.
  const early = accountThree([1, 2, 2], [200, 120, 125]);
  assert.match(early.problems.join("\n"), /ui-component started before ui-unit ended/u);
  assert.match(early.problems.join("\n"), /ui-heavy started before ui-unit ended/u);
  assert.equal(early.status, "incomplete");
});

test("a category run accounts its own project only", () => {
  const accounting = account({ accounted: ["heavy"], reportText: reportOf(["src/c.test.tsx"]) });
  assert.equal(accounting.status, "complete");
  assert.deepEqual(accounting.projects.map((row) => row.category), ["ui-heavy"]);
  const leaked = account({ accounted: ["heavy"], reportText: reportOf(sampleBaseline) });
  assert.equal(leaked.status, "incomplete", "files of another project ran in a category run");
});

test("the summary names each project, its lane and an unknown outcome as unknown", () => {
  const lines = accountingSummaryLines([
    { name: "vitest", accounting: account({ reportText: reportOf(sampleBaseline) }) },
    { name: "missing", accounting: account({ reportError: "the runner wrote no Vitest report" }) },
  ]);
  assert.deepEqual(lines, [
    "vitest projects: complete",
    "  ui-unit: passed: 2/2 files, 2 passed, 0 failed, 0 skipped",
    "  ui-heavy [serialized lane]: passed: 1/1 files, 1 passed, 0 failed, 0 skipped",
    "missing projects: unknown (the runner wrote no Vitest report)",
    "  ui-unit: unknown: 2 files selected, outcome unknown",
    "  ui-heavy [serialized lane]: unknown: 1 files selected, outcome unknown",
  ]);
});
