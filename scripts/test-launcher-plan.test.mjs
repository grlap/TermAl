// Pins the full gate's plan as another host's run records it
// (test-launcher.mjs `fullGatePlan`): built only from the host-bound values a
// run records, with that host's path rules, so a gate run on a Mac compares
// exactly from Windows and the other way round. It launches nothing.
// New file; the launcher's own test file is already large.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { fullGatePlan, requiredStages } from "./test-launcher.mjs";
import { removeFixtureOnPass } from "./test-fixture-cleanup.mjs";
import { testTempDirectory } from "./test-temp-root.mjs";

const launcher = fileURLToPath(new URL("./test-launcher.mjs", import.meta.url));
const names = ["cargo-check", "typescript", "fingerprint-tests", "rust-tests", "vitest"];
const bindings = (resolved) => Object.fromEntries(names.map((name) => [name, resolved[name]]));

const mac = {
  platform: "darwin",
  node: "/opt/homebrew/bin/node",
  root: "/Users/runner/TermAl/.tmp/wt-ref",
  runDir: "/Users/runner/TermAl/.git/worktrees/wt-ref/review-runs/test-mac",
  reporterUrl: "file:///Users/runner/TermAl/.tmp/wt-ref/scripts/node-test-duration-reporter.mjs",
  executables: bindings({
    "cargo-check": "/Users/runner/.cargo/bin/cargo",
    typescript: "/opt/homebrew/bin/node",
    "fingerprint-tests": "/opt/homebrew/bin/node",
    "rust-tests": "/bin/sh",
    vitest: "/opt/homebrew/bin/node",
  }),
};

const windows = {
  platform: "win32",
  node: "C:\\Program Files\\nodejs\\node.exe",
  gitBash: "C:\\Program Files\\Git\\bin\\bash.exe",
  root: "F:\\GitHub\\TermAl\\.tmp\\wt-ref",
  runDir: "F:\\GitHub\\TermAl\\.git\\worktrees\\wt-ref\\review-runs\\test-win",
  reporterUrl: "file:///F:/GitHub/TermAl/.tmp/wt-ref/scripts/node-test-duration-reporter.mjs",
  executables: bindings({
    "cargo-check": "C:\\Users\\runner\\.cargo\\bin\\cargo.exe",
    typescript: "C:\\Program Files\\nodejs\\node.exe",
    "fingerprint-tests": "C:\\Program Files\\nodejs\\node.exe",
    "rust-tests": "C:\\Program Files\\Git\\bin\\bash.exe",
    vitest: "C:\\Program Files\\nodejs\\node.exe",
  }),
};

const entry = (plan, name) => plan.entries.find((candidate) => candidate.name === name);

test("full gate plan: a Mac run's stages and entries use that run's values and POSIX paths", () => {
  const plan = fullGatePlan(mac);
  assert.deepEqual(plan.stages, requiredStages("darwin", {}, mac.node));
  assert.deepEqual(plan.entries.map(({ name }) => name), names);
  assert.deepEqual(entry(plan, "cargo-check"),
    { name: "cargo-check", command: ["/Users/runner/.cargo/bin/cargo", "check"], cwd: mac.root });
  assert.deepEqual(entry(plan, "rust-tests"),
    { name: "rust-tests", command: ["/bin/sh", "scripts/test-rust.sh"], cwd: mac.root });
  assert.equal(entry(plan, "typescript").cwd, `${mac.root}/ui`);
  const vitest = entry(plan, "vitest").command;
  assert.equal(vitest.at(-1), `--outputFile.json=${mac.runDir}/vitest-durations.json`);
  const helpers = entry(plan, "fingerprint-tests").command;
  assert.ok(helpers.includes(`--test-reporter=${mac.reporterUrl}`));
  assert.ok(helpers.includes(`--test-reporter-destination=${mac.runDir}/fingerprint-tests-durations.jsonl`));
});

test("full gate plan: a Windows run's stages and entries use that run's values and Windows paths", () => {
  const plan = fullGatePlan(windows);
  assert.deepEqual(plan.stages,
    requiredStages("win32", { TERMAL_TEST_GIT_BASH: windows.gitBash }, windows.node));
  assert.deepEqual(entry(plan, "rust-tests").command,
    [windows.gitBash, "-c", "exec scripts/test-rust.sh"]);
  assert.equal(entry(plan, "typescript").cwd, `${windows.root}\\ui`);
  assert.equal(entry(plan, "vitest").command.at(-1),
    "--outputFile.json=F:/GitHub/TermAl/.git/worktrees/wt-ref/review-runs/test-win/vitest-durations.json");
  assert.ok(entry(plan, "fingerprint-tests").command.includes(
    `--test-reporter-destination=${windows.runDir}\\fingerprint-tests-durations.jsonl`));
});

test("full gate plan: the plan never depends on the host computing it", () => {
  // The same input gives the same plan here whatever this host is; only the
  // input's platform chooses the path rules.
  assert.notDeepEqual(fullGatePlan(mac).entries, fullGatePlan({ ...mac, platform: "linux", root: "/srv/wt" }).entries);
  assert.deepEqual(fullGatePlan(mac), fullGatePlan(structuredClone(mac)));
});

test("full gate plan: missing host-bound values are refused, never filled from this host", () => {
  assert.throws(() => fullGatePlan({ ...mac, platform: "beos" }), /plan platform/u);
  assert.throws(() => fullGatePlan({ ...windows, gitBash: undefined }), /needs the run's gitBash/u);
  for (const key of ["node", "root", "runDir", "reporterUrl"]) {
    assert.throws(() => fullGatePlan({ ...mac, [key]: "" }), new RegExp(`plan ${key}`, "u"));
  }
  const { "rust-tests": _omitted, ...partial } = mac.executables;
  assert.throws(() => fullGatePlan({ ...mac, executables: partial }), /no recorded executable for stage rust-tests/u);
});

test("full gate plan: the CLI prints the same plan and runs no stage", (t) => {
  const directory = mkdtempSync(join(testTempDirectory(), "launcher-plan-"));
  removeFixtureOnPass(t, directory);
  const input = join(directory, "input.json");
  writeFileSync(input, JSON.stringify(mac));
  const printed = execFileSync(process.execPath, [launcher, "plan", input],
    { encoding: "utf8", windowsHide: true });
  assert.deepEqual(JSON.parse(printed), fullGatePlan(mac));
});
