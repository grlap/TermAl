// Exercises the shared test-launcher contract without running product gates.
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  closeSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, isAbsolute, join, relative } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  categoryRequiredFiles,
  categoryStage,
  createRun,
  diagnostics,
  ensureCargoTargetDirectory,
  executeRun,
  helperTestFiles,
  liveStage,
  notifyRun,
  RENAME_SHARING_BUDGET_MS,
  RENAME_SHARING_FIRST_BACKOFF_MS,
  RENAME_SHARING_MAX_BACKOFF_MS,
  recoverRun,
  replaceAtomically,
  requiredFiles,
  requiredStages,
  runCommand,
  save,
  startDetachedRun,
  summarize,
} from "./test-launcher.mjs";
import { isolatedGitEnvironment } from "./review-freeze-fingerprint.mjs";
import { focusedTestCounts } from "./test-counts.mjs";
import { testTempDirectory } from "./test-temp-root.mjs";

const fixtureEnv = {
  ...process.env,
  TERMAL_SESSION_ID: "fixture-owner",
  TERMAL_CLI: process.execPath,
};
const launcherScript = fileURLToPath(new URL("./test-launcher.mjs", import.meta.url));
const projectRoot = fileURLToPath(new URL("../", import.meta.url));
const stage = (name, source = "") => ({
  name,
  command: process.execPath,
  args: ["-e", source],
});
const json = (path) => JSON.parse(readFileSync(path, "utf8"));
const logPath = (runDir, entry) => isAbsolute(entry.log) ? entry.log : join(runDir, entry.log);

async function within(promise, label, milliseconds = 10_000) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${label} did not complete within ${milliseconds}ms`)), milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function repository(t, callback, sourceEnv = fixtureEnv, directory = testTempDirectory()) {
  const root = mkdtempSync(join(directory, "launcher-fixture-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const env = isolatedGitEnvironment(sourceEnv);
  const nullDevice = process.platform === "win32" ? "NUL" : "/dev/null";
  execFileSync("git", ["init", "--quiet", "--template="], { cwd: root, env });
  for (const [name, value] of [
    ["user.email", "termal-test@example.invalid"],
    ["user.name", "TermAl Test"],
    ["core.autocrlf", "false"],
    ["core.hooksPath", nullDevice],
  ]) {
    execFileSync("git", ["config", name, value], { cwd: root, env });
  }
  writeFileSync(join(root, "tracked.txt"), "baseline\n");
  execFileSync("git", ["add", "tracked.txt"], { cwd: root, env });
  execFileSync("git", ["commit", "--quiet", "--no-gpg-sign", "-m", "baseline"], {
    cwd: root,
    env,
  });
  await callback(root, env);
}

async function isolatedDetachedRepository(t, callback) {
  await repository(t, async (root, env) => {
    const notificationMarker = join(root, ".git", "notifications.jsonl");
    writeFileSync(join(root, "mailbox"), [
      "const fs = require('node:fs');",
      "const path = require('node:path');",
      "const marker = process.env.FIXTURE_NOTIFICATION_MARKER;",
      "const expected = Number(process.env.FIXTURE_NOTIFICATION_BARRIER || '1');",
      "fs.writeFileSync(path.join(path.dirname(marker), `notify-ready-${process.pid}`), 'ready');",
      "const deadline = Date.now() + 5000;",
      "while (fs.readdirSync(path.dirname(marker)).filter((name) => name.startsWith('notify-ready-')).length < expected) {",
      "  if (Date.now() >= deadline) throw new Error('notification fixture barrier timed out');",
      "  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 10);",
      "}",
      "const args = process.argv.slice(2);",
      "const messageFile = args[args.indexOf('--message-file') + 1];",
      "fs.appendFileSync(marker, `${JSON.stringify({ args, body: fs.readFileSync(messageFile, 'utf8') })}\\n`);",
    ].join("\n"));
    await callback(
      root,
      { ...env, FIXTURE_NOTIFICATION_MARKER: notificationMarker },
      notificationMarker,
    );
  });
}

test("required stages preserve the TermAl five-gate order and direct JavaScript entrypoints", () => {
  for (const platform of ["win32", "linux"]) {
    const stages = requiredStages(platform, { ...process.env, ProgramFiles: "C:\\Program Files" });
    assert.deepEqual(stages.map(({ name }) => name), [
      "cargo-check",
      "typescript",
      "fingerprint-tests",
      "rust-tests",
      "vitest",
    ]);
    assert.deepEqual([stages[0].command, ...stages[0].args], ["cargo", "check"]);
    assert.deepEqual(stages[1].args, ["node_modules/typescript/bin/tsc", "--noEmit"]);
    assert.equal(stages[1].cwd, "ui");
    assert.deepEqual(stages[2].args, ["--test", ...helperTestFiles]);
    assert.match([stages[3].command, ...stages[3].args].join(" "), platform === "win32"
      ? /Git[\\/]bin[\\/]bash\.exe.*-c.*scripts\/test-rust\.sh/u
      : /^sh scripts\/test-rust\.sh$/u);
    assert.deepEqual(stages[4].args, ["node_modules/vitest/vitest.mjs", "run"]);
    assert.equal(stages[4].cwd, "ui");
    assert.deepEqual(stages.map(({ durationReport }) => durationReport), [
      undefined,
      undefined,
      "node-test-events",
      "unavailable",
      "vitest-json",
    ]);
    assert.match([liveStage(platform, { ...process.env, ProgramFiles: "C:\\Program Files" }).command,
      ...liveStage(platform, { ...process.env, ProgramFiles: "C:\\Program Files" }).args].join(" "),
    /test-rust\.sh.*root_recovery_live.*--ignored.*--test-threads=1/u);
  }
});

test("full plan prerequisites cover every maintained helper and direct UI entrypoint", () => {
  const files = new Set(requiredFiles(projectRoot));
  for (const path of [...helperTestFiles,
    "scripts/node-test-duration-reporter.mjs",
    "scripts/review-freeze-fingerprint.mjs",
    "scripts/test-counts.mjs",
    "scripts/test-durations.mjs",
    "scripts/test-launcher.mjs",
    "scripts/test-temp-root.mjs",
    "scripts/vitest-resource-preflight.mjs",
    "scripts/test-rust.sh"]) {
    assert(files.has(join(projectRoot, path)), `missing prerequisite ${path}`);
  }
  for (const stage of requiredStages()) {
    if (stage.command === process.execPath && stage.args[0]?.startsWith("node_modules/")) {
      assert(files.has(join(projectRoot, stage.cwd, stage.args[0])));
    }
  }
});

test("a stage's duration report is recorded beside its log and summarized without deciding its state", async (t) => {
  await repository(t, async (root, env) => {
    writeFileSync(join(root, "fixture.test.mjs"), [
      "import test from 'node:test';",
      "test('fast fixture', () => {});",
      "test('failing fixture', () => { throw new Error('expected failure'); });",
      "",
    ].join("\n"));
    execFileSync("git", ["add", "fixture.test.mjs"], { cwd: root, env });
    // The stage's runner must not inherit this runner's child-protocol marker,
    // or it streams events to a parent that is not listening.
    const { NODE_TEST_CONTEXT: _parentRunner, ...runEnv } = fixtureEnv;
    const runDir = await createRun({
      root,
      stages: [{
        name: "helpers",
        command: process.execPath,
        args: ["--test", "fixture.test.mjs"],
        durationReport: "node-test-events",
      }],
    }, runEnv);
    const result = await executeRun(runDir, runEnv);
    const [entry] = result.stages;
    assert.equal(entry.state, "failed", "the failing fixture still fails the stage");
    assert.ok(entry.command.includes("--test-reporter=spec"));
    assert.ok(entry.command.includes(`--test-reporter-destination=${join(runDir, "helpers-durations.jsonl")}`));
    assert.match(readFileSync(logPath(runDir, entry), "utf8"), /✖ failing fixture/u);
    assert.equal(entry.durations.status, "measured");
    assert.equal(entry.durations.tests, 2);
    assert.equal(entry.durations.budgetMs, 2000);
    assert.deepEqual(json(join(runDir, "results.json")).stages[0].durations, entry.durations);
    const summary = await summarize(runDir);
    assert.match(summary, /^helpers: failed exit=1/mu);
    assert.match(summary, /^durations: a test's time is its runner's figure, which includes its own per-test hooks/mu);
    assert.match(summary, /^helpers: \d+ of 2 tests >= 2000 ms/mu);
  });
});

test("a passing stage stays passed whatever its duration artifact holds", async (t) => {
  await repository(t, async (root, env) => {
    // A stage script that receives the report arguments and writes whatever
    // the case asks into the artifact path it is given, then exits 0.
    writeFileSync(join(root, "stage.mjs"), [
      "import { writeFileSync } from 'node:fs';",
      "const target = process.argv.find((arg) => arg.startsWith('--outputFile.json='));",
      "if (process.env.STAGE_ARTIFACT && target) {",
      "  writeFileSync(target.slice('--outputFile.json='.length), process.env.STAGE_ARTIFACT);",
      "}",
      "",
    ].join("\n"));
    execFileSync("git", ["add", "stage.mjs"], { cwd: root, env });
    for (const [label, artifact, status] of [
      ["no artifact", "", "missing"],
      ["malformed artifact", "{not json", "unreadable"],
      ["wrong shape", JSON.stringify({ results: [] }), "unreadable"],
    ]) {
      const runEnv = { ...fixtureEnv, STAGE_ARTIFACT: artifact };
      const runDir = await createRun({
        root,
        stages: [{ name: "ui", command: process.execPath, args: ["stage.mjs"], durationReport: "vitest-json" }],
      }, runEnv);
      const result = await executeRun(runDir, runEnv);
      assert.equal(result.state, "passed", label);
      assert.equal(result.exitCode, 0, label);
      assert.equal(result.stages[0].state, "passed", label);
      assert.equal(result.stages[0].durations.status, status, label);
      assert.match(await summarize(runDir), /^PASS /u, label);
    }
  });
});

test("the full gate accounts every UI project, and a category runs one under the same configuration", () => {
  const vitest = requiredStages().find(({ name }) => name === "vitest");
  assert.equal(vitest.uiProjects, "all");
  assert.equal(vitest.durationReport, "vitest-json");
  const heavy = { name: "heavy", groupOrder: 3 };
  const category = categoryStage(heavy);
  assert.equal(category.name, "ui-heavy");
  assert.equal(category.cwd, vitest.cwd);
  assert.equal(category.command, vitest.command);
  // Only the project selector differs: the same vite.config.ts applies.
  assert.deepEqual(category.args, [...vitest.args, "--project", "heavy"]);
  assert.deepEqual(category.uiProjects, ["heavy"]);
  assert.equal(category.durationReport, "vitest-json");
  for (const path of ["scripts/test-categories-plan.mjs", "ui/test-categories.ts", "ui/node_modules/vitest/vitest.mjs"]) {
    assert.ok(categoryRequiredFiles(projectRoot).includes(join(projectRoot, path)), path);
  }
  assert.ok(requiredFiles(projectRoot).includes(join(projectRoot, "ui", "test-categories.ts")));
});

// A repository with a two-project UI manifest and a stage that writes a
// Vitest JSON report naming whichever files STAGE_FILES lists.
async function uiProjectRepository(t, callback) {
  await repository(t, async (root, env) => {
    mkdirSync(join(root, "ui", "src"), { recursive: true });
    writeFileSync(join(root, "ui", "test-categories.ts"), [
      "export type CategoryProject = { readonly name: string; readonly include: readonly string[]; readonly exclude: readonly string[]; readonly groupOrder: number };",
      "export const CATEGORY_PROJECTS: readonly CategoryProject[] = [",
      "  { name: \"unit\", include: [\"src/a.test.ts\"], exclude: [], groupOrder: 1 },",
      "  { name: \"heavy\", include: [\"src/c.test.tsx\"], exclude: [], groupOrder: 2 },",
      "];",
      "export function projectSelects(project: CategoryProject, file: string): boolean {",
      "  return project.include.includes(file) && !project.exclude.includes(file);",
      "}",
      "",
    ].join("\n"));
    writeFileSync(join(root, "ui", "src", "a.test.ts"), "");
    writeFileSync(join(root, "ui", "src", "c.test.tsx"), "");
    writeFileSync(join(root, "ui", "stage.mjs"), [
      "import { writeFileSync } from 'node:fs';",
      "import { resolve } from 'node:path';",
      "const target = process.argv.find((arg) => arg.startsWith('--outputFile.json='));",
      "const files = (process.env.STAGE_FILES ?? '').split(',').filter(Boolean);",
      "if (target && process.env.STAGE_NO_REPORT !== '1') {",
      "  let clock = 1000;",
      "  writeFileSync(target.slice('--outputFile.json='.length), JSON.stringify({ testResults: files.map((file) => {",
      "    const startTime = clock; clock += 10;",
      "    return { name: resolve(file).replaceAll(String.fromCharCode(92), '/'), status: 'passed', startTime,",
      "      endTime: startTime + 10, assertionResults: [{ fullName: file, status: 'passed' }] };",
      "  }) }));",
      "}",
      "",
    ].join("\n"));
    execFileSync("git", ["add", "ui"], { cwd: root, env });
    await callback(root);
  });
}

const uiStage = (overrides = {}) => ({
  name: "vitest",
  command: process.execPath,
  args: ["stage.mjs"],
  cwd: "ui",
  durationReport: "vitest-json",
  uiProjects: "all",
  ...overrides,
});

test("a Vitest stage that exits 0 still fails when a UI project did not run or cannot be accounted", async (t) => {
  await uiProjectRepository(t, async (root) => {
    for (const [label, env, status, accounting, row] of [
      ["every project ran", { STAGE_FILES: "src/a.test.ts,src/c.test.tsx" }, "passed", "complete", "ui-heavy [serialized lane]: passed: 1/1 files"],
      ["the heavy project ran nothing", { STAGE_FILES: "src/a.test.ts" }, "failed", "incomplete", "ui-heavy [serialized lane]: empty"],
      ["no report was written", { STAGE_NO_REPORT: "1" }, "failed", "unknown", "ui-heavy [serialized lane]: unknown"],
    ]) {
      const runEnv = { ...fixtureEnv, ...env };
      const runDir = await createRun({ root, stages: [uiStage()] }, runEnv);
      const result = await executeRun(runDir, runEnv);
      const [entry] = result.stages;
      assert.equal(entry.code, 0, `${label}: the runner itself exited 0`);
      assert.equal(entry.state, status, label);
      assert.equal(result.state, status, label);
      assert.equal(result.exitCode, status === "passed" ? 0 : 1, label);
      assert.equal(entry.accounting.status, accounting, label);
      assert.deepEqual(entry.accounting.projects.map((child) => [child.kind, child.category]), [
        ["test", "ui-unit"],
        ["test", "ui-heavy"],
      ], label);
      assert.deepEqual(json(join(runDir, "results.json")).stages[0].accounting, entry.accounting, label);
      const summary = await summarize(runDir);
      assert.match(summary, status === "passed" ? /^PASS /u : /^FAIL /u, label);
      assert.match(summary, new RegExp(`^vitest projects: ${accounting}`, "mu"), label);
      assert.ok(summary.includes(`  ${row}`), `${label}: ${summary}`);
      if (status === "failed") assert.match(entry.error, /UI project accounting/u, label);
    }
  });
});

test("a category run is a partial check in its request, results and summary", async (t) => {
  await uiProjectRepository(t, async (root) => {
    const runEnv = { ...fixtureEnv, STAGE_FILES: "src/c.test.tsx" };
    const scope = { kind: "category", category: "ui-heavy", project: "heavy", label: "partial check, not the full gate" };
    const runDir = await createRun({
      root,
      stages: [uiStage({ name: "ui-heavy", uiProjects: ["heavy"] })],
      scope,
    }, runEnv);
    const request = json(join(runDir, "request.json"));
    assert.equal(request.full, false, "a category is never the full gate");
    assert.deepEqual(request.scope, scope);
    const result = await executeRun(runDir, runEnv);
    assert.equal(result.state, "passed");
    assert.deepEqual(result.scope, scope);
    assert.deepEqual(result.stages[0].accounting.projects.map((child) => child.category), ["ui-heavy"]);
    const summary = await summarize(runDir);
    assert.match(summary, /^scope: category ui-heavy \(Vitest project heavy\): partial check, not the full gate$/mu);
    assert.ok(summary.indexOf("scope:") < summary.indexOf("ui-heavy projects:"), "the scope comes first");
  });
});

test("the category command refuses a missing or unknown name before any run exists", () => {
  for (const [args, message] of [
    [["category"], /category requires a NAME/u],
    [["category", "ui-bogus"], /unknown category: ui-bogus; known categories: ui-unit, ui-component, ui-heavy, ui-app/u],
    [["category", "--detach"], /category requires a NAME/u],
    [["category", "ui-heavy", "--", "echo"], /category takes only NAME, --notify and --detach/u],
  ]) {
    let failure;
    try {
      execFileSync(process.execPath, [launcherScript, ...args], { env: fixtureEnv, encoding: "utf8", stdio: "pipe" });
    } catch (error) {
      failure = error;
    }
    assert.ok(failure, `${args.join(" ")} must fail`);
    assert.match(`${failure.stdout}${failure.stderr}`, message, args.join(" "));
    assert.doesNotMatch(`${failure.stdout}${failure.stderr}`, /^(?:RUN|STARTED) /mu, `${args.join(" ")}: no run`);
  }
});

test("a request naming an unknown duration report kind is rejected before any stage runs", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({
      root,
      stages: [{ ...stage("guess", "process.exit(0)"), durationReport: "guess" }],
    }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.state, "failed");
    assert.match(result.error, /invalid stage/u);
    assert.deepEqual(result.stages.map(({ state }) => state), ["unrun"]);
  });
});

test("configured shell preflight executes a portable command instead of requesting --version", async (t) => {
  await repository(t, async (root) => {
    const shell = requiredStages(process.platform, fixtureEnv)
      .find(({ name }) => name === "rust-tests").command;
    const runDir = await createRun({
      root,
      stages: [{ name: "rust-tests", command: shell, args: ["-c", "exit 0"] }],
      needsCargo: true,
    }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.state, "passed");
    assert.deepEqual(result.preflight.map(({ name }) => name), ["cargo", "test-shell"]);
    assert.deepEqual(result.preflight[1].command.slice(1), ["-c", "exit 0"]);
    assert.equal(result.preflight[1].code, 0);
  });
});

test("explicit Cargo and Rust toolchain selections are preserved and recorded", async (t) => {
  await repository(t, async (root, gitEnv) => {
    const env = {
      ...gitEnv,
      TERMAL_TEST_CARGO: process.execPath,
      RUSTUP_TOOLCHAIN: "nightly-fixture",
    };
    const assertion = [
      "const assert = require('node:assert/strict');",
      `assert.equal(process.env.TERMAL_TEST_CARGO, ${JSON.stringify(process.execPath.replaceAll("\\", "/"))});`,
      "assert.equal(process.env.RUSTUP_TOOLCHAIN, 'nightly-fixture');",
    ].join("");
    const runDir = await createRun({
      root,
      stages: [{ name: "cargo-check", command: "cargo", args: ["-e", assertion] }],
      needsCargo: true,
    }, env);
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "passed");
    assert.deepEqual(result.cargo, {
      path: process.execPath,
      source: "TERMAL_TEST_CARGO",
    });
    assert.equal(result.stages[0].command[0], process.execPath);
  });
});

test("a fresh worktree gets its ignored Cargo build directory before any stage", async (t) => {
  await repository(t, async (root, gitEnv) => {
    const env = { ...gitEnv, TERMAL_TEST_CARGO: process.execPath };
    delete env.CARGO_TARGET_DIR;
    delete env.CARGO_BUILD_TARGET_DIR;
    // What the first Cargo stage finds: the tagged directory or none, and
    // never a sibling left for Cargo to make and rename.
    const finds = (tagged) => [
      "const assert = require('node:assert/strict');",
      "const fs = require('node:fs');",
      `assert.equal(fs.existsSync('target/CACHEDIR.TAG'), ${tagged});`,
      "assert.deepEqual(fs.readdirSync('.').filter((name) => /^target.+/u.test(name)), []);",
    ].join("");
    const run = async (tagged, runEnv = env) => {
      const runDir = await createRun({
        root,
        stages: [{ name: "cargo-check", command: "cargo", args: ["-e", finds(tagged)] }],
        needsCargo: true,
      }, runEnv);
      return executeRun(runDir, runEnv);
    };
    // Git does not ignore it here: making it would change the frozen input.
    assert.equal((await run(false)).state, "passed");
    assert.equal(existsSync(join(root, "target")), false);

    writeFileSync(join(root, ".gitignore"), "/target\n");
    execFileSync("git", ["add", ".gitignore"], { cwd: root, env: gitEnv });
    execFileSync("git", ["commit", "--quiet", "--no-gpg-sign", "-m", "ignore target"], {
      cwd: root,
      env: gitEnv,
    });
    // A build directory set elsewhere is Cargo's to make.
    const elsewhere = { ...env, CARGO_TARGET_DIR: join(root, "elsewhere") };
    assert.equal((await run(false, elsewhere)).state, "passed");
    assert.equal(existsSync(join(root, "target")), false);

    // Ignored, as in a Rust repository: made and tagged before the first
    // stage, and the frozen input still matches before and after.
    const result = await run(true);
    assert.equal(result.state, "passed");
    assert.equal(result.before, result.expectedFingerprint);
    assert.equal(result.after, result.expectedFingerprint);
    assert.match(
      readFileSync(join(root, "target", "CACHEDIR.TAG"), "utf8"),
      /^Signature: 8a477f597d28d172789f06886806bc55\n/u,
    );

    // An existing one is left as it is.
    writeFileSync(join(root, "target", "CACHEDIR.TAG"), "kept\n");
    assert.equal(ensureCargoTargetDirectory(root, env), false);
    assert.equal(readFileSync(join(root, "target", "CACHEDIR.TAG"), "utf8"), "kept\n");
  });
});

test("focused runs neither inject a Rust toolchain nor require ambient Cargo", async (t) => {
  await repository(t, async (root, gitEnv) => {
    const env = { ...gitEnv };
    delete env.RUSTUP_TOOLCHAIN;
    const runDir = await createRun({
      root,
      stages: [stage(
        "toolchain-unset",
        "require('node:assert/strict').equal(process.env.RUSTUP_TOOLCHAIN, undefined)",
      )],
    }, env);
    assert.equal((await executeRun(runDir, env)).state, "passed");

    const missingCargoRun = await createRun({
      root,
      stages: [stage("cargo-check")],
      needsCargo: true,
    }, env);
    const withoutCargo = { ...env };
    for (const key of Object.keys(withoutCargo)) {
      if (key.toLowerCase() === "path") delete withoutCargo[key];
    }
    withoutCargo.PATH = dirname(process.execPath);
    delete withoutCargo.TERMAL_TEST_CARGO;
    const missing = await executeRun(missingCargoRun, withoutCargo);
    assert.equal(missing.state, "failed");
    assert.match(missing.error, /executable not found: cargo/u);
    assert.deepEqual(missing.stages.map(({ state }) => state), ["unrun"]);
  });
});

test("pinned live execution receives explicit Cargo, binary, and SHA without running live tests", async (t) => {
  await repository(t, async (root, gitEnv) => {
    const expectedSha256 = createHash("sha256").update(readFileSync(process.execPath)).digest("hex");
    const env = { ...gitEnv, TERMAL_TEST_CARGO: process.execPath };
    const assertion = [
      "const assert = require('node:assert/strict');",
      `assert.equal(process.env.TERMAL_TEST_CARGO, ${JSON.stringify(process.execPath.replaceAll("\\", "/"))});`,
      `assert.equal(process.env.TERMAL_TEST_LIVE_ENGRAM_BINARY, ${JSON.stringify(process.execPath)});`,
      `assert.equal(process.env.TERMAL_TEST_LIVE_ENGRAM_SHA256, ${JSON.stringify(expectedSha256)});`,
    ].join("");
    const runDir = await createRun({
      root,
      stages: [{ name: "engram-live", command: process.execPath, args: ["-e", assertion] }],
      needsCargo: true,
      liveEngram: { path: process.execPath, expectedSha256 },
    }, env);
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "passed");
    assert.equal(result.liveEngram.observedSha256, expectedSha256);
    assert.equal(result.stages[0].command[0], process.execPath);
  });
});

test("stage cwd controls the real child directory and relative file lookup", async (t) => {
  await repository(t, async (root) => {
    const ui = join(root, "ui");
    mkdirSync(ui);
    writeFileSync(join(ui, "relative-fixture.txt"), "ui-relative\n");
    const source = `
      const assert = require('node:assert/strict');
      const fs = require('node:fs');
      const path = require('node:path');
      // The temp directory may be reached through an alias (macOS /var is a
      // link to /private/var), and the child sees the resolved spelling.
      assert.equal(fs.realpathSync.native(process.cwd()), fs.realpathSync.native(${JSON.stringify(ui)}));
      assert.equal(fs.readFileSync('relative-fixture.txt', 'utf8'), 'ui-relative\\n');
      assert.equal(path.basename(process.cwd()), 'ui');
    `;
    const runDir = await createRun({
      root,
      stages: [{ ...stage("ui-cwd", source), cwd: "ui" }],
    }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.state, "passed");
    assert.equal(realpathSync.native(result.stages[0].cwd), realpathSync.native(ui));
    assert.equal(result.stages[0].code, 0);
  });
});

test("launcher fixtures and fingerprints ignore inherited Git config, hooks, and retargeting", async (t) => {
  const hostileRoot = mkdtempSync(join(testTempDirectory(), "launcher-hostile-git-"));
  t.after(() => rmSync(hostileRoot, { recursive: true, force: true }));
  const hooks = join(hostileRoot, "hooks");
  mkdirSync(hooks);
  const hook = join(hooks, "pre-commit");
  writeFileSync(hook, "#!/bin/sh\necho invoked > inherited-hook-ran\n");
  chmodSync(hook, 0o755);
  const globalConfig = join(hostileRoot, "gitconfig");
  writeFileSync(globalConfig, `[core]\n\thooksPath = ${hooks.replaceAll("\\", "/")}\n`);
  const poisoned = {
    ...fixtureEnv,
    GIT_CONFIG_GLOBAL: globalConfig,
    GIT_CONFIG_COUNT: "1",
    GIT_CONFIG_KEY_0: "core.hooksPath",
    GIT_CONFIG_VALUE_0: hooks.replaceAll("\\", "/"),
    GIT_DIR: join(hostileRoot, "retargeted.git"),
    GIT_WORK_TREE: hostileRoot,
    GIT_INDEX_FILE: join(hostileRoot, "retargeted.index"),
  };
  await repository(t, async (root) => {
    assert.equal(existsSync(join(root, "inherited-hook-ran")), false);
    const runDir = await createRun({
      root,
      stages: [stage("clean")],
    }, poisoned);
    assert.equal((await executeRun(runDir, poisoned)).state, "passed");
    assert.equal(existsSync(join(root, "inherited-hook-ran")), false);
    assert.equal(existsSync(poisoned.GIT_INDEX_FILE), false);
  }, poisoned);
});

test("clean pass retains logs and summarizes without passing-test lists", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({
      root,
      stages: [stage("clean", "console.log('ok 1 - passing-marker')")],
    }, fixtureEnv);
    // The fixture root comes from the temp directory, which may be spelled
    // through an alias (a Windows CI runner's 8.3 RUNNER~1, macOS /var for
    // /private/var), while the run directory comes from Git's canonical path:
    // compare canonical spellings.
    assert.match(
      relative(realpathSync.native(join(root, ".git", "review-runs")), realpathSync.native(runDir)),
      /^test-[^\\/]+$/u,
    );
    assert.equal(json(join(runDir, "results.json")).state, "running");
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.state, "passed");
    assert.equal(result.exitCode, 0);
    assert.equal(result.failureInvestigation, undefined);
    assert.match(readFileSync(logPath(runDir, result.stages[0]), "utf8"), /passing-marker/u);
    const summary = await summarize(runDir);
    assert.match(summary, /PASS/u);
    assert.doesNotMatch(summary, /INVESTIGATION REQUIRED/u);
    assert.doesNotMatch(summary, /passing-marker/u);
    assert.ok(Buffer.byteLength(summary) < 12_288);
  });
});

test("nonzero exit is preserved and later stages remain unrun", async (t) => {
  await repository(t, async (root) => {
    const marker = join(root, ".git", "should-not-run");
    const runDir = await createRun({ root, stages: [
      stage("failure", "console.error('error: deliberate fixture failure'); process.exit(23)"),
      stage("later", `require('node:fs').writeFileSync(${JSON.stringify(marker)}, 'ran')`),
    ] }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.state, "failed");
    assert.equal(result.exitCode, 23);
    assert.deepEqual(result.stages.map(({ state }) => state), ["failed", "unrun"]);
    assert.equal(result.failureInvestigation.status, "required");
    assert.equal(result.failureInvestigation.runDir, runDir);
    assert.match(result.failureInvestigation.action, /falsifiable hypotheses/u);
    assert.match(result.failureInvestigation.retryPolicy, /passing retry is not a diagnosis or closure/u);
    assert.equal(existsSync(marker), false);
    const summary = await summarize(runDir);
    assert.match(summary, /INVESTIGATION REQUIRED/u);
    assert.match(summary, /deliberate fixture failure/u);
    assert.ok(summary.indexOf("INVESTIGATION REQUIRED") < summary.indexOf("deliberate fixture failure"));
  });
});

test("warnings survive while passing Rust and TAP lines stay out of diagnostics", async (t) => {
  await repository(t, async (root) => {
    const source = [
      "test success_test ... ok",
      "test result: ok. 1026 passed; 0 failed; 4 ignored",
      "# Subtest: reports an error and warning correctly",
      "ok 1 - reports an error and warning correctly",
      "warning: actual compiler caution",
      "test next_success ... ok",
      "test result: ok. 50 passed; 0 failed; 0 ignored",
    ].join("\n");
    const runDir = await createRun({
      root,
      stages: [stage("report", `console.log(${JSON.stringify(source)})`)],
    }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.state, "passed");
    assert.equal(result.stages[0].diagnostics.text, "warning: actual compiler caution\n");
    assert.doesNotMatch(await summarize(runDir), /1026 passed|50 passed|success_test|next_success|Subtest/u);
  });
});

test("passing Vitest rows cannot consume diagnostics before a genuine later failure", async (t) => {
  await repository(t, async (root) => {
    const passing = Array.from({ length: 100 }, (_, index) =>
      ` \u001b[32m${index % 2 ? "✔" : "✓"}\u001b[39m fixture.test.ts > handles error and warning passing-marker-${index} 2ms`);
    const failure = [
      " FAIL fixture.test.ts > real failure",
      "Error: actual failure marker",
      "  at fixture.test.ts:5:7",
    ];
    const source = [...passing, ...failure, " ✓ fixture.test.ts > another passing-marker 1ms"].join("\n");
    const runDir = await createRun({ root, stages: [stage(
      "vitest-report",
      `console.log(${JSON.stringify(source)}); process.exitCode = 19`,
    )] }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.exitCode, 19);
    assert.equal(result.stages[0].diagnostics.text, `${failure.join("\n")}\n`);
    assert.equal(result.stages[0].diagnostics.truncated, false);
    assert.doesNotMatch(await summarize(runDir), /passing-marker/u);
    assert.match(readFileSync(result.stages[0].log, "utf8"), /passing-marker-99/u);
  });
});

test("passing rows do not erase bounded context from an active failure", async (t) => {
  await repository(t, async (root) => {
    const failure = [
      "thread 'interleaved' panicked at fixture.rs:7:9:",
      "assertion `left == right` failed",
      "  left: 1",
      " right: 2",
    ];
    const passing = "test unrelated_success ... ok";
    const source = [failure[0], passing, ...failure.slice(1)].join("\n");
    const runDir = await createRun({ root, stages: [stage(
      "interleaved-failure",
      `console.log(${JSON.stringify(source)}); process.exitCode = 13`,
    )] }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.exitCode, 13);
    assert.equal(result.stages[0].diagnostics.text, `${failure.join("\n")}\n`);
    assert.doesNotMatch(await summarize(runDir), /unrelated_success/u);
    assert.match(readFileSync(result.stages[0].log, "utf8"), /unrelated_success/u);
  });
});

test("passing rows stay out of failure fallback tails and full logs remain intact", async (t) => {
  await repository(t, async (root) => {
    const passing = Array.from({ length: 100 }, (_, index) =>
      ` ✓ fixture.test.ts > error warning passing-marker-${index} ${"x".repeat(40)}`);
    const source = [...passing, "plain unexplained failure marker"].join("\n");
    const runDir = await createRun({ root, stages: [stage(
      "fallback-report",
      `console.log(${JSON.stringify(source)}); process.exitCode = 7`,
    )] }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    const entry = result.stages[0];
    assert.equal(result.exitCode, 7);
    assert.equal(entry.diagnostics.text, "plain unexplained failure marker\n");
    assert.equal(entry.diagnostics.fallback, "no recognized diagnostic; bounded failure tail");
    assert.equal(entry.diagnostics.truncated, true);
    assert.doesNotMatch(await summarize(runDir), /passing-marker/u);
    assert.match(readFileSync(entry.log, "utf8"), /passing-marker-99/u);
  });
});

test("Node warning headers remain visible without passing names", async (t) => {
  await repository(t, async (root) => {
    const warnings = [
      `(node:${process.pid}) [DEP0040] DeprecationWarning: fixture deprecation`,
      `(node:${process.pid}) ExperimentalWarning: fixture experiment`,
      `# (node:${process.pid}) [CUSTOM] MaxListenersExceededWarning: fixture listeners`,
    ];
    const output = [...warnings, "ok 1 - ExperimentalWarning passing name", " ✓ DeprecationWarning passing name"].join("\n");
    const runDir = await createRun({
      root,
      stages: [stage("node-warnings", `console.error(${JSON.stringify(output)})`)],
    }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.exitCode, 0);
    assert.equal(result.stages[0].diagnostics.text, `${warnings.join("\n")}\n`);
    assert.doesNotMatch(await summarize(runDir), /passing name/u);
  });
});

test("failure diagnostics take priority over earlier warning-heavy stages", async (t) => {
  await repository(t, async (root) => {
    const warnings = "warning: earlier caution " + "w".repeat(3000);
    const runDir = await createRun({ root, stages: [
      ...[1, 2, 3].map((index) => stage(`noisy-${index}`, `console.error(${JSON.stringify(warnings)})`)),
      stage("broken", "console.error('error: essential failure detail'); process.exitCode = 17"),
    ] }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.exitCode, 17);
    assert.deepEqual(result.stages.map(({ state }) => state), ["passed", "passed", "passed", "failed"]);
    const summary = await summarize(runDir);
    assert.match(summary, /error: essential failure detail/u);
    assert.ok(summary.indexOf("essential failure detail") < summary.indexOf("earlier caution"));
    assert.match(summary, /summary diagnostics truncated/u);
    assert.ok(Buffer.byteLength(summary) < 12_288);
  });
});

test("long logical-line fragments cannot become diagnostic headers", async (t) => {
  await repository(t, async (root) => {
    const log = join(root, ".git", "long-line.log");
    for (const ending of ["error: false fragment", "ok 2 - false fragment"]) {
      const output = `ok 1 - ${"x".repeat(30_000)}${ending}\nerror: actual next line\n  genuine context\n`;
      writeFileSync(log, output);
      assert.deepEqual(await diagnostics(log, false), {
        text: "error: actual next line\n  genuine context\n",
        truncated: true,
      });
      assert.equal(readFileSync(log, "utf8"), output);
    }
  });
});

test("missing commands and prerequisite files fail before every stage", async (t) => {
  await repository(t, async (root) => {
    for (const request of [
      { stages: [stage("would-pass"), { name: "missing", command: join(root, "missing-executable"), args: [] }] },
      { stages: [stage("would-pass")], prerequisiteFiles: [join(root, "missing-file")] },
    ]) {
      const runDir = await createRun({ root, ...request }, fixtureEnv);
      const result = await executeRun(runDir, fixtureEnv);
      assert.equal(result.state, "failed");
      assert.deepEqual(result.stages.map(({ state }) => state), request.stages.map(() => "unrun"));
    }
  });
});

test("runtime spawn exceptions persist terminal failure", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({ root, stages: [
      { name: "invalid-argument", command: process.execPath, args: ["\0"] },
      stage("later"),
    ] }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.state, "failed");
    assert.ok(result.error);
    assert.deepEqual(result.stages.map(({ state }) => state), ["failed", "unrun"]);
    assert.equal(json(join(runDir, "results.json")).state, "failed");
  });
});

test("large diagnostic output is bounded while the complete log survives", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({ root, stages: [stage(
      "large",
      "require('node:fs').writeSync(2, 'error: ' + 'x'.repeat(2 * 1024 * 1024) + '\\nTAIL-MARKER\\n'); process.exitCode = 4",
    )] }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    const entry = result.stages[0];
    assert.equal(result.exitCode, 4);
    assert.equal(entry.diagnostics.truncated, true);
    assert.ok(Buffer.byteLength(entry.diagnostics.text) < 12_288);
    assert.ok(statSync(logPath(runDir, entry)).size > 2 * 1024 * 1024);
    assert.match(readFileSync(logPath(runDir, entry), "utf8"), /TAIL-MARKER/u);
    assert.ok(Buffer.byteLength(await summarize(runDir)) < 12_288);
  });
});

test("source drift before and during execution cannot pass", async (t) => {
  await repository(t, async (root) => {
    const before = await createRun({ root, stages: [stage("unused")] }, fixtureEnv);
    writeFileSync(join(root, "tracked.txt"), "changed before execution\n");
    const refused = await executeRun(before, fixtureEnv);
    assert.equal(refused.stages[0].state, "unrun");
    assert.match(refused.error, /drift.*changed fingerprint components:/u);
    assert.match(refused.error, /trackedHeadDiffSha256/u);
    assert.match(refused.error, /statusSha256/u);
    assert.doesNotMatch(refused.error, /tracked\.txt/u);

    writeFileSync(join(root, "tracked.txt"), "baseline\n");
    const during = await createRun({ root, stages: [stage(
      "changes-source",
      "require('node:fs').writeFileSync('tracked.txt', 'changed during execution\\n')",
    )] }, fixtureEnv);
    const result = await executeRun(during, fixtureEnv);
    assert.equal(result.state, "failed");
    assert.match(result.error, /drift.*changed fingerprint components:/u);
    assert.match(result.error, /trackedHeadDiffSha256/u);
    assert.match(result.error, /statusSha256/u);
    assert.doesNotMatch(result.error, /tracked\.txt/u);
  });
});

test("incomplete runs report UNKNOWN and cannot notify", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({
      root,
      stages: [stage("not-started")],
      notifyTo: "fixture-parent",
    }, fixtureEnv);
    assert.match(await summarize(runDir), /UNKNOWN/u);
    let sent = false;
    await assert.rejects(
      notifyRun(runDir, fixtureEnv, async () => { sent = true; return { code: 0 }; }),
      /terminal/u,
    );
    assert.equal(sent, false);
  });
});

test("execution lock prevents rerunning a terminal run", async (t) => {
  await repository(t, async (root) => {
    const counter = join(root, ".git", "execution-count");
    const runDir = await createRun({ root, stages: [stage(
      "count-once",
      `require('node:fs').appendFileSync(${JSON.stringify(counter)}, 'run\\n')`,
    )] }, fixtureEnv);
    await executeRun(runDir, fixtureEnv);
    const resultBytes = readFileSync(join(runDir, "results.json"), "utf8");
    await assert.rejects(executeRun(runDir, fixtureEnv), /EEXIST|exist/u);
    assert.equal(readFileSync(counter, "utf8"), "run\n");
    assert.equal(readFileSync(join(runDir, "results.json"), "utf8"), resultBytes);
  });
});

test("failed-run notification carries investigation handoff and retries without rerunning", async (t) => {
  await repository(t, async (root) => {
    const counter = join(root, ".git", "execution-count");
    const runDir = await createRun({
      root,
      stages: [stage("count-once", `require('node:fs').appendFileSync(${JSON.stringify(counter)}, 'run\\n'); console.error('error: diagnose fixture'); process.exitCode = 9`)],
      notifyTo: "fixture-parent",
    }, fixtureEnv);
    await executeRun(runDir, fixtureEnv);
    const resultBytes = readFileSync(join(runDir, "results.json"), "utf8");
    const calls = [];
    const send = async (command, args, options) => {
      assert.equal(json(join(runDir, "results.json")).state, "failed");
      assert.equal(command, process.execPath);
      assert.equal(options.env.TERMAL_SESSION_ID, "fixture-owner");
      calls.push(args);
      return { code: calls.length === 1 ? 7 : 0, signal: null };
    };
    await assert.rejects(notifyRun(runDir, fixtureEnv, send), /tests were NOT rerun/u);
    const message = readFileSync(join(runDir, "notification.message.txt"), "utf8");
    assert.match(message, /^FAIL/u);
    assert.match(message, /INVESTIGATION REQUIRED/u);
    assert.match(message, /passing retry is not a diagnosis or closure/u);
    assert.match(message, /error: diagnose fixture/u);
    await notifyRun(runDir, fixtureEnv, send);
    assert.deepEqual(calls[1], calls[0]);
    const args = calls[0];
    assert.equal(args[args.indexOf("--to") + 1], "fixture-parent");
    assert.equal(args[args.indexOf("--idempotency-key") + 1], `termal-tests:${basename(runDir)}`);
    assert.equal(readFileSync(join(runDir, "notification.message.txt"), "utf8"), message);
    assert.equal(readFileSync(join(runDir, "results.json"), "utf8"), resultBytes);
    assert.equal(readFileSync(counter, "utf8"), "run\n");
  });
});

test("separate notify-only processes publish an intact receipt without rerunning", async (t) => {
  await isolatedDetachedRepository(t, async (root, env, notificationMarker) => {
    const counter = join(root, ".git", "execution-count");
    const runDir = await createRun({
      root,
      stages: [stage(
        "count-once",
        `require('node:fs').appendFileSync(${JSON.stringify(counter)}, 'run\\n')`,
      )],
      notifyTo: "Termal::Codex",
    }, env);
    assert.equal((await executeRun(runDir, env)).state, "passed");
    const expectedMessage = await summarize(runDir);
    const notificationLogs = Array.from(
      { length: 4 },
      (_, index) => join(runDir, `notify-process-${index + 1}.log`),
    );
    const invoke = (log) => runCommand(
      process.execPath,
      [launcherScript, "notify", runDir],
      { cwd: root, env: { ...env, FIXTURE_NOTIFICATION_BARRIER: "4" }, log },
    );
    const outcomes = await within(
      Promise.all(notificationLogs.map(invoke)),
      "concurrent notify-only processes",
    );
    assert.deepEqual(
      outcomes,
      notificationLogs.map(() => ({ code: 0, signal: null })),
      notificationLogs.map((log) => readFileSync(log, "utf8").slice(-1200)).join("\n"),
    );
    assert.equal(readFileSync(counter, "utf8"), "run\n");
    const calls = readFileSync(notificationMarker, "utf8")
      .trim()
      .split("\n")
      .map((line) => JSON.parse(line));
    assert.equal(calls.length, 4);
    for (const call of calls) assert.equal(call.body, expectedMessage);
    for (const call of calls.slice(1)) assert.deepEqual(call.args, calls[0].args);
    const args = calls[0].args;
    const messageFile = args[args.indexOf("--message-file") + 1];
    assert.equal(messageFile, join(runDir, "notification.message.txt"));
    assert.equal(
      args[args.indexOf("--idempotency-key") + 1],
      `termal-tests:${basename(runDir)}`,
    );
    assert.equal(json(join(runDir, "notification.json")).code, 0);
    assert.equal(
      readdirSync(runDir).filter((name) => /^notification-receipt-.*\.json$/u.test(name)).length,
      4,
    );
    assert.equal(readFileSync(messageFile, "utf8"), expectedMessage);
  });
});

test("invalid notification prerequisites fail before a run is created", async (t) => {
  await isolatedDetachedRepository(t, async (root, env) => {
    const runRoot = join(root, ".git", "review-runs");
    for (const [notifyTo, changedEnv, expected] of [
      ["   ", env, /target.*nonempty/iu],
      ["--pretend-option", env, /look like an option/iu],
      ["fixture-owner", env, /self-send/iu],
      ["Termal::Codex", { ...env, TERMAL_SESSION_ID: " " }, /TERMAL_SESSION_ID|sender/iu],
      ["Termal::Codex", { ...env, TERMAL_CLI: "relative-cli" }, /absolute executable/iu],
    ]) {
      await assert.rejects(
        createRun({ root, stages: [stage("unused")], notifyTo }, changedEnv),
        expected,
      );
      assert.equal(existsSync(runRoot), false);
    }
  });
});

test("changed notification sender is rejected without resending", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({
      root,
      stages: [stage("clean")],
      notifyTo: "fixture-parent",
    }, fixtureEnv);
    await executeRun(runDir, fixtureEnv);
    let sent = false;
    const send = async () => { sent = true; return { code: 0 }; };
    await assert.rejects(
      notifyRun(runDir, { ...fixtureEnv, TERMAL_SESSION_ID: "different-owner" }, send),
      /session|sender|owner/iu,
    );
    assert.equal(sent, false);
  });
});

test("detached worker reaches readiness, runs once, and sends one notification", async (t) => {
  await isolatedDetachedRepository(t, async (root, env, notificationMarker) => {
    const counter = join(root, ".git", "execution-count");
    const runDir = await createRun({
      root,
      stages: [stage(
        "count-once",
        `require('node:fs').appendFileSync(${JSON.stringify(counter)}, 'run\\n')`,
      )],
      notifyTo: "  Termal::Codex  ",
    }, env);
    assert.equal(json(join(runDir, "request.json")).notifyTo, "Termal::Codex");
    const detached = await within(startDetachedRun(runDir, env), "detached readiness");
    assert.ok(Number.isInteger(detached.pid));
    assert.deepEqual(await within(detached.completion, "detached completion"), {
      code: 0,
      signal: null,
    });
    assert.equal(json(join(runDir, "results.json")).state, "passed");
    assert.equal(readFileSync(counter, "utf8"), "run\n");
    const notifications = readFileSync(notificationMarker, "utf8").trim().split("\n");
    assert.equal(notifications.length, 1);
    const { args, body } = JSON.parse(notifications[0]);
    assert.equal(args[args.indexOf("--to") + 1], "Termal::Codex");
    assert.equal(args[args.indexOf("--idempotency-key") + 1], `termal-tests:${basename(runDir)}`);
    assert.equal(body, await summarize(runDir));
  });
});

test("a run records whether it is detached and which process created it", async (t) => {
  await repository(t, async (root) => {
    const foreground = json(join(await createRun({ root, stages: [stage("clean")] }, fixtureEnv), "request.json"));
    assert.equal(foreground.detached, false);
    assert.equal(foreground.creatorPid, process.pid);
    const runDir = await createRun({ root, stages: [stage("clean")], detached: true }, fixtureEnv);
    const detached = json(join(runDir, "request.json"));
    assert.equal(detached.detached, true);
    assert.equal(detached.creatorPid, process.pid);
    // The fields are metadata for TermAl's index; the worker ignores them.
    assert.equal((await executeRun(runDir, fixtureEnv)).state, "passed");
  });
});

test("detached lock contention closes before readiness without changing results", async (t) => {
  await isolatedDetachedRepository(t, async (root, env, notificationMarker) => {
    const counter = join(root, ".git", "execution-count");
    const runDir = await createRun({
      root,
      stages: [stage(
        "must-not-run",
        `require('node:fs').appendFileSync(${JSON.stringify(counter)}, 'run\\n')`,
      )],
      notifyTo: "Termal::Codex",
    }, env);
    const resultBytes = readFileSync(join(runDir, "results.json"), "utf8");
    writeFileSync(join(runDir, "execution.lock"), "owned\n");
    await assert.rejects(startDetachedRun(runDir, env), /closed before readiness.*exist/isu);
    assert.equal(readFileSync(join(runDir, "results.json"), "utf8"), resultBytes);
    assert.equal(existsSync(counter), false);
    assert.equal(existsSync(notificationMarker), false);
  });
});

test("detached corrupt request fails visibly without inventing a notification route", async (t) => {
  await isolatedDetachedRepository(t, async (root, env, notificationMarker) => {
    const runDir = await createRun({
      root,
      stages: [stage("must-not-run")],
      notifyTo: "Termal::Codex",
    }, env);
    const request = json(join(runDir, "request.json"));
    request.runId = "forged-run-id";
    writeFileSync(join(runDir, "request.json"), `${JSON.stringify(request, null, 2)}\n`);
    await assert.rejects(startDetachedRun(runDir, env), /closed before readiness/iu);
    const result = json(join(runDir, "results.json"));
    assert.equal(result.state, "failed");
    assert.match(result.error, /startup failed before readiness/iu);
    assert.equal(result.notificationEligible, false);
    assert.equal(existsSync(notificationMarker), false);
  });
});

test("detached corrupt results persist failure and notify before closing unready", async (t) => {
  await isolatedDetachedRepository(t, async (root, env, notificationMarker) => {
    const runDir = await createRun({
      root,
      stages: [stage("must-not-run")],
      notifyTo: "Termal::Codex",
    }, env);
    writeFileSync(join(runDir, "results.json"), "{not-json\n");
    await assert.rejects(startDetachedRun(runDir, env), /closed before readiness/iu);
    const result = json(join(runDir, "results.json"));
    assert.equal(result.state, "failed");
    assert.match(result.error, /startup failed before readiness/iu);
    assert.equal(result.notificationEligible, true);
    assert.equal(result.stages[0].state, "unrun");
    assert.equal(readFileSync(notificationMarker, "utf8").trim().split("\n").length, 1);
  });
});

test("live binary hash mismatch is terminal and precedes the stage", async (t) => {
  await repository(t, async (root) => {
    const actual = createHash("sha256").update(readFileSync(process.execPath)).digest("hex");
    const wrong = `${actual[0] === "0" ? "1" : "0"}${actual.slice(1)}`;
    const runDir = await createRun({
      root,
      stages: [stage("live-would-run")],
      liveEngram: { path: process.execPath, expectedSha256: wrong },
    }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.state, "failed");
    assert.equal(result.stages[0].state, "unrun");
    assert.match(result.error, /SHA-256 mismatch/u);
    assert.equal(result.liveEngram.observedSha256, actual);
  });
});

test("an admitted worker that cannot save its terminal result sends one UNKNOWN notice under its own key", async (t) => {
  await isolatedDetachedRepository(t, async (root, env, notificationMarker) => {
    // The stage turns results.json into a directory, so every later save fails.
    const runDir = await createRun({
      root,
      stages: [stage("break-results", [
        "const fs = require('node:fs');",
        "const path = require('node:path');",
        "const base = path.join('.git', 'review-runs');",
        "const [run] = fs.readdirSync(base);",
        "const results = path.join(base, run, 'results.json');",
        "fs.rmSync(results);",
        "fs.mkdirSync(results);",
      ].join(" "))],
      notifyTo: "Termal::Codex",
    }, env);
    const detached = await within(startDetachedRun(runDir, env), "detached readiness");
    assert.deepEqual(await within(detached.completion, "detached completion"), {
      code: 1,
      signal: null,
    });
    const calls = readFileSync(notificationMarker, "utf8")
      .trim()
      .split("\n")
      .map((line) => JSON.parse(line));
    assert.equal(calls.length, 1);
    const { args, body } = calls[0];
    assert.equal(args[args.indexOf("--to") + 1], "Termal::Codex");
    assert.equal(
      args[args.indexOf("--idempotency-key") + 1],
      `termal-tests:${basename(runDir)}:runner-error`,
    );
    assert.match(body, /^UNKNOWN /u);
    assert.match(body, /Nothing here is a PASS/u);
    assert.match(body, /recover/u);
    assert.match(body, /recover fails the same way and the run stays UNKNOWN/u);
    assert.equal(existsSync(join(runDir, "notification.json")), false);
  });
});

test("recover refuses a live worker and a run no worker ever owned", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({
      root,
      stages: [stage("unrun")],
      notifyTo: "fixture-parent",
    }, fixtureEnv);
    let sent = false;
    const send = async () => { sent = true; return { code: 0, signal: null }; };
    await assert.rejects(recoverRun(runDir, fixtureEnv, { send }), /no process ever took ownership/u);
    const resultPath = join(runDir, "results.json");
    const result = json(resultPath);
    result.pid = process.pid;
    writeFileSync(resultPath, `${JSON.stringify(result, null, 2)}\n`);
    const bytes = readFileSync(resultPath, "utf8");
    await assert.rejects(
      recoverRun(runDir, fixtureEnv, { send }),
      new RegExp(`worker pid ${process.pid} may still be running`, "u"),
    );
    assert.equal(readFileSync(resultPath, "utf8"), bytes);
    assert.equal(existsSync(join(runDir, "recovery.lock")), false);
    assert.equal(sent, false);
  });
});

test("recover settles a dead worker's run as interrupted, and only the owner notifies, once", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({
      root,
      stages: [stage("done"), stage("interrupted"), stage("never-started")],
      notifyTo: "fixture-parent",
    }, fixtureEnv);
    const resultPath = join(runDir, "results.json");
    const running = json(resultPath);
    running.pid = 4242;
    Object.assign(running.stages[0], { state: "passed", code: 0 });
    running.stages[1].state = "running";
    writeFileSync(resultPath, `${JSON.stringify(running, null, 2)}\n`);
    const calls = [];
    const send = async (command, args) => {
      calls.push(args);
      return { code: 0, signal: null };
    };
    const gone = () => false;

    // The waiting coordinator may settle the run but cannot speak for its owner.
    const coordinator = { ...fixtureEnv, TERMAL_SESSION_ID: "fixture-parent" };
    assert.deepEqual(await recoverRun(runDir, coordinator, { send, isAlive: gone }), {
      settled: true,
      notification: "not sent: only the session that owns the run may send its completion",
    });
    const settled = json(resultPath);
    assert.equal(settled.state, "failed");
    assert.equal(settled.exitCode, 1);
    assert.equal(settled.interrupted, true);
    assert.match(settled.error, /worker 4242 exited without saving a terminal result/u);
    assert.deepEqual(settled.stages.map(({ state }) => state), ["passed", "failed", "unrun"]);
    assert.match(settled.stages[1].error, /outcome is unknown/u);
    assert.equal(settled.failureInvestigation.status, "required");
    assert.match(await summarize(runDir), /^FAIL /u);
    assert.equal(calls.length, 0);

    assert.deepEqual(await recoverRun(runDir, fixtureEnv, { send, isAlive: gone }), {
      settled: false,
      notification: "sent",
    });
    assert.equal(calls.length, 1);
    assert.equal(
      calls[0][calls[0].indexOf("--idempotency-key") + 1],
      `termal-tests:${basename(runDir)}`,
    );
    assert.match(readFileSync(join(runDir, "notification.message.txt"), "utf8"), /interrupted/u);

    const bytes = readFileSync(resultPath, "utf8");
    assert.deepEqual(await recoverRun(runDir, fixtureEnv, { send, isAlive: gone }), {
      settled: false,
      notification: "already delivered",
    });
    assert.equal(calls.length, 1);
    assert.equal(readFileSync(resultPath, "utf8"), bytes);
  });
});

test("helper scripts run when started through a linked directory", async (t) => {
  // macOS reaches its temp directory through /var -> /private/var, and a
  // Windows junction is the same kind of alias. Node resolves a module's own
  // URL through links while argv keeps the caller's spelling, so an entry
  // guard comparing them lexically makes the script exit silently.
  // The link targets a private copy of the scripts, so no cleanup can reach
  // the checkout through it.
  const base = mkdtempSync(join(testTempDirectory(), "launcher-link-"));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const real = join(base, "real");
  mkdirSync(join(real, "scripts"), { recursive: true });
  for (const name of ["test-launcher.mjs", "review-freeze-fingerprint.mjs", "test-temp-root.mjs", "test-durations.mjs", "node-test-duration-reporter.mjs", "test-categories-plan.mjs", "test-counts.mjs"]) {
    copyFileSync(join(projectRoot, "scripts", name), join(real, "scripts", name));
  }
  const linked = join(base, "linked");
  symlinkSync(real, linked, "junction");
  const run = (name, args = [], cwd = base) => new Promise((resolveRun) => {
    const child = spawn(process.execPath, [join(linked, "scripts", name), ...args], {
      cwd, env: fixtureEnv, stdio: ["ignore", "pipe", "pipe"], windowsHide: true,
    });
    let output = "";
    child.stdout.on("data", (chunk) => { output += chunk; });
    child.stderr.on("data", (chunk) => { output += chunk; });
    child.once("close", (code) => resolveRun({ code, output }));
  });
  const launcher = await within(run("test-launcher.mjs"), "launcher through link");
  assert.match(launcher.output, /FAIL launcher: usage:/u, launcher.output);
  assert.equal(launcher.code, 1);
  const tempRoot = await within(run("test-temp-root.mjs"), "temp root through link");
  assert.match(tempRoot.output, /Usage: node scripts\/test-temp-root\.mjs/u, tempRoot.output);
  assert.equal(tempRoot.code, 2);
  await repository(t, async (root) => {
    const fingerprint = await within(run("review-freeze-fingerprint.mjs", [], root), "fingerprint through link");
    assert.match(fingerprint.output, /^headCommit=[0-9a-f]{40}$/mu, fingerprint.output);
    assert.equal(fingerprint.code, 0);
  });
});

test("a foreground run prints its run receipt before any stage completes", async (t) => {
  await repository(t, async (root, env) => {
    // The launcher roots its runs at its own repository, so run a copy of it
    // from inside the fixture.
    const scripts = join(root, "scripts");
    mkdirSync(scripts);
    for (const name of ["test-launcher.mjs", "review-freeze-fingerprint.mjs", "test-temp-root.mjs", "test-durations.mjs", "node-test-duration-reporter.mjs", "test-categories-plan.mjs", "test-counts.mjs"]) {
      copyFileSync(join(projectRoot, "scripts", name), join(scripts, name));
    }
    const gate = mkdtempSync(join(testTempDirectory(), "launcher-receipt-"));
    t.after(() => rmSync(gate, { recursive: true, force: true }));
    const release = join(gate, "release");
    const child = spawn(process.execPath, [
      join(scripts, "test-launcher.mjs"),
      "focused",
      "--",
      process.execPath,
      "-e",
      [
        "const fs = require('node:fs');",
        "const deadline = Date.now() + 10000;",
        `while (!fs.existsSync(${JSON.stringify(release)})) {`,
        "  if (Date.now() > deadline) process.exit(3);",
        "  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 20);",
        "}",
      ].join(" "),
    ], { cwd: root, env, stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
    let stdout = "";
    let stderr = "";
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    const exited = new Promise((resolveExit) => child.once("close", (code) => resolveExit(code)));
    try {
      const receipt = await within(new Promise((resolveReceipt, rejectReceipt) => {
        child.stdout.on("data", (chunk) => {
          stdout += chunk;
          if (stdout.includes("\n")) resolveReceipt(stdout.split("\n")[0].trim());
        });
        child.once("close", () => {
          rejectReceipt(new Error(`launcher exited before a receipt: ${stdout}${stderr}`));
        });
      }), "foreground receipt");
      const match = /^RUN (.+)$/u.exec(receipt);
      assert.ok(match, receipt);
      const runDir = match[1];
      // The stage cannot finish before `release` exists, so no verdict may
      // have been printed yet. results.json is not read here, so the test
      // cannot race the launcher's own replacement of it.
      assert.doesNotMatch(stdout, /^(?:PASS|FAIL) /mu, stdout);
      // request.json is final once createRun returns, before the receipt.
      const request = json(join(runDir, "request.json"));
      assert.equal(request.detached, false);
      assert.equal(request.creatorPid, child.pid, "the foreground launcher runs its own stages");
      writeFileSync(release, "go\n");
      assert.equal(await within(exited, "foreground completion"), 0, `${stdout}${stderr}`);
      assert.match(stdout, new RegExp(`^RUN .+\\r?\\n(?:.*\\r?\\n)*PASS ${basename(runDir)} exit=0`, "u"));
    } finally {
      // Never leave the launcher running in the fixture directory, which the
      // fixture removes next: release its stage and wait for it to end.
      if (!existsSync(release)) writeFileSync(release, "go\n");
      await within(exited, "launcher shutdown");
    }
  });
});

// Rewrites a run's results.json in place, as a fixture for recovery states.
function rewriteResult(runDir, change) {
  const path = join(runDir, "results.json");
  const result = json(path);
  change(result);
  writeFileSync(path, `${JSON.stringify(result, null, 2)}\n`);
  return result;
}

const recordingSend = (calls) => async (command, args) => {
  calls.push(args);
  return { code: 0, signal: null };
};

test("recover keeps a terminal result the worker saved after recovery first read the run", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({
      root,
      stages: [stage("finishing")],
      notifyTo: "fixture-parent",
    }, fixtureEnv);
    const running = rewriteResult(runDir, (result) => {
      result.pid = 4242;
      result.stages[0].state = "running";
    });
    const calls = [];
    // The worker saves its terminal result between recovery's first read and
    // its liveness check, then exits.
    const finishesThenExits = () => {
      writeFileSync(join(runDir, "results.json"), `${JSON.stringify({
        ...running,
        state: "passed",
        exitCode: 0,
        ended: new Date().toISOString(),
        stages: [{ ...running.stages[0], state: "passed", code: 0 }],
      }, null, 2)}\n`);
      return false;
    };
    assert.deepEqual(
      await recoverRun(runDir, fixtureEnv, { send: recordingSend(calls), isAlive: finishesThenExits }),
      { settled: false, notification: "sent" },
    );
    const kept = json(join(runDir, "results.json"));
    assert.equal(kept.state, "passed");
    assert.equal(kept.interrupted, undefined);
    assert.equal(calls.length, 1);
    assert.match(readFileSync(join(runDir, "notification.message.txt"), "utf8"), /^PASS /u);
    assert.equal(existsSync(join(runDir, "recovery.lock")), false);
  });
});

test("a recovery whose settlement write fails releases its lock and can be retried", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({
      root,
      stages: [stage("interrupted")],
      notifyTo: "fixture-parent",
    }, fixtureEnv);
    rewriteResult(runDir, (result) => {
      result.pid = 4242;
      result.stages[0].state = "running";
    });
    const calls = [];
    let failures = 1;
    const failingOnce = (path, value) => {
      if (failures > 0) {
        failures -= 1;
        throw Object.assign(new Error("fixture: disk full"), { code: "ENOSPC" });
      }
      writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
    };
    const options = { send: recordingSend(calls), isAlive: () => false, saveResult: failingOnce };
    await assert.rejects(recoverRun(runDir, fixtureEnv, options), /disk full/u);
    assert.equal(existsSync(join(runDir, "recovery.lock")), false);
    assert.equal(json(join(runDir, "results.json")).state, "running");
    assert.equal(calls.length, 0);
    assert.deepEqual(await recoverRun(runDir, fixtureEnv, options), {
      settled: true,
      notification: "sent",
    });
    assert.equal(json(join(runDir, "results.json")).state, "failed");
    assert.equal(calls.length, 1);
  });
});

test("recover leaves a recovery lock it does not own and settles nothing behind it", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({
      root,
      stages: [stage("interrupted")],
      notifyTo: "fixture-parent",
    }, fixtureEnv);
    const running = rewriteResult(runDir, (result) => {
      result.pid = 4242;
      result.stages[0].state = "running";
    });
    const lock = join(runDir, "recovery.lock");
    writeFileSync(lock, "another recovery\n");
    const calls = [];
    await assert.rejects(
      recoverRun(runDir, fixtureEnv, { send: recordingSend(calls), isAlive: () => false }),
      /another recovery of this run is in progress/u,
    );
    assert.equal(existsSync(lock), true);
    assert.equal(json(join(runDir, "results.json")).state, "running");
    // Once the other recovery has saved a terminal result, this one sends it.
    const settledElsewhere = () => {
      writeFileSync(join(runDir, "results.json"), `${JSON.stringify({
        ...running,
        state: "failed",
        exitCode: 1,
        interrupted: true,
        error: "interrupted: settled by the other recovery",
        ended: new Date().toISOString(),
      }, null, 2)}\n`);
      return false;
    };
    assert.deepEqual(
      await recoverRun(runDir, fixtureEnv, { send: recordingSend(calls), isAlive: settledElsewhere }),
      { settled: false, notification: "sent" },
    );
    assert.equal(existsSync(lock), true);
    assert.equal(calls.length, 1);
  });
});

test("a run whose request failed validation gets no completion from notify or recover", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({
      root,
      stages: [stage("never-run")],
      notifyTo: "fixture-parent",
    }, fixtureEnv);
    rewriteResult(runDir, (result) => {
      Object.assign(result, {
        state: "failed",
        exitCode: 1,
        ended: new Date().toISOString(),
        error: "worker startup failed before readiness: fixture",
        notificationEligible: false,
      });
    });
    const calls = [];
    await assert.rejects(notifyRun(runDir, fixtureEnv, recordingSend(calls)), /failed validation/u);
    assert.deepEqual(
      await recoverRun(runDir, fixtureEnv, { send: recordingSend(calls), isAlive: () => false }),
      { settled: false, notification: "not sent: the run's request failed validation" },
    );
    assert.equal(calls.length, 0);
    assert.equal(existsSync(join(runDir, "notification.message.txt")), false);
  });
});

test("recover without a notification route settles and says nothing was requested", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({ root, stages: [stage("interrupted")] }, fixtureEnv);
    rewriteResult(runDir, (result) => {
      result.pid = 4242;
      result.stages[0].state = "running";
    });
    const calls = [];
    assert.deepEqual(
      await recoverRun(runDir, fixtureEnv, { send: recordingSend(calls), isAlive: () => false }),
      { settled: true, notification: "not requested" },
    );
    assert.equal(calls.length, 0);
  });
});

test("a worker refused admission never speaks for the run", async (t) => {
  await isolatedDetachedRepository(t, async (root, env, notificationMarker) => {
    const runDir = await createRun({
      root,
      stages: [stage("must-not-run")],
      notifyTo: "Termal::Codex",
    }, env);
    const resultBytes = readFileSync(join(runDir, "results.json"), "utf8");
    writeFileSync(join(runDir, "execution.lock"), "owned\n");
    const log = join(root, ".git", "unadmitted-worker.log");
    const outcome = await within(
      runCommand(process.execPath, [launcherScript, "_run", runDir], { cwd: root, env, log }),
      "unadmitted worker",
    );
    assert.equal(outcome.code, 1);
    assert.match(readFileSync(log, "utf8"), /failed before admission/u);
    assert.equal(existsSync(notificationMarker), false);
    assert.equal(readFileSync(join(runDir, "results.json"), "utf8"), resultBytes);
  });
});

test("an admitted worker that fails after a terminal result was saved sends that result, not an UNKNOWN notice", async (t) => {
  if (process.platform !== "win32") {
    // The fault must fail the follow-up save while results.json stays
    // readable and the run directory writable (admission creates
    // execution.lock there). POSIX replaces a read-only file freely; Windows
    // refuses to, which is exactly that fault.
    t.skip("needs Windows' refusal to replace a read-only results.json");
    return;
  }
  await isolatedDetachedRepository(t, async (root, env, notificationMarker) => {
    const runDir = await createRun({
      root,
      stages: [stage("must-not-run")],
      notifyTo: "Termal::Codex",
    }, env);
    // A run is already terminal when its worker starts if creation failed it.
    rewriteResult(runDir, (result) => {
      Object.assign(result, {
        state: "failed",
        exitCode: 1,
        ended: new Date().toISOString(),
        error: "fixture: failed at creation",
        notificationEligible: true,
      });
    });
    const resultPath = join(runDir, "results.json");
    const resultBytes = readFileSync(resultPath, "utf8");
    // Run as a plain child, the worker has no readiness IPC channel, so its
    // readiness fails after admission. The read-only results.json then
    // refuses the readiness-failure annotation, and the worker errors with
    // the creation failure still saved.
    chmodSync(resultPath, 0o444);
    const log = join(root, ".git", "readiness-worker.log");
    let outcome;
    try {
      outcome = await within(
        runCommand(process.execPath, [launcherScript, "_run", runDir], { cwd: root, env, log }),
        "worker whose readiness fails",
      );
    } finally {
      chmodSync(resultPath, 0o644);
    }
    assert.equal(outcome.code, 1);
    const output = readFileSync(log, "utf8");
    assert.match(output, /runner error after its terminal result was saved/u);
    assert.doesNotMatch(output, /no terminal result was saved/u);
    assert.equal(readFileSync(resultPath, "utf8"), resultBytes);
    const calls = readFileSync(notificationMarker, "utf8")
      .trim()
      .split("\n")
      .map((line) => JSON.parse(line));
    assert.equal(calls.length, 1, "one completion and no runner-error notice");
    const { args, body } = calls[0];
    assert.equal(args[args.indexOf("--to") + 1], "Termal::Codex");
    assert.equal(args[args.indexOf("--idempotency-key") + 1], `termal-tests:${basename(runDir)}`);
    assert.equal(body, await summarize(runDir));
  });
});

test("the recover command reports an already terminal run without changing it", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({ root, stages: [stage("clean")] }, fixtureEnv);
    assert.equal((await executeRun(runDir, fixtureEnv)).state, "passed");
    const resultPath = join(runDir, "results.json");
    const resultBytes = readFileSync(resultPath, "utf8");
    const log = join(root, ".git", "recover-command.log");
    const outcome = await within(
      runCommand(process.execPath, [launcherScript, "recover", runDir], {
        cwd: root,
        env: fixtureEnv,
        log,
      }),
      "recover command",
    );
    const output = readFileSync(log, "utf8");
    assert.equal(outcome.code, 0, output);
    assert.match(output, /^PASS /mu);
    assert.match(output, /Already terminal; notification not requested; tests not rerun\./u);
    assert.equal(readFileSync(resultPath, "utf8"), resultBytes);
  });
});

test("CI runs every maintained helper test suite on each platform", () => {
  const workflow = readFileSync(
    join(projectRoot, ".github", "workflows", "review-freeze.yml"),
    "utf8",
  );
  const run = /run: node --test (.+)$/mu.exec(workflow);
  assert.ok(run, "the workflow runs node --test");
  assert.deepEqual(run[1].trim().split(/\s+/u), [...helperTestFiles]);
  assert.match(workflow, /os: \[ubuntu-latest, macos-latest, windows-latest\]/u);
});

// On Windows a rename that replaces a file fails with EPERM while any other
// handle on that file is open, even one that shares delete; a handle this
// process holds stands for another process's reader, such as the host's.
const windowsSharing = process.platform === "win32" ? false : "Windows file-sharing semantics";

test("save() replaces a results file another handle holds open once that handle closes", { skip: windowsSharing }, (t) => {
  const directory = mkdtempSync(join(testTempDirectory(), "launcher-save-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const target = join(directory, "results.json");
  writeFileSync(target, `${JSON.stringify({ state: "running" })}\n`);
  let held = openSync(target, "r");
  const sleeps = [];
  try {
    save(target, { state: "passed" }, {
      sleep: (milliseconds) => {
        sleeps.push(milliseconds);
        if (held !== null) {
          // The first backoff releases this test's handle. A later one means
          // another process (a scanner) briefly holds the new file too; it
          // waits as the real sleep would.
          closeSync(held);
          held = null;
        } else {
          Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, milliseconds);
        }
      },
    });
  } finally {
    if (held !== null) closeSync(held);
  }
  assert.deepEqual(json(target), { state: "passed" });
  assert.equal(sleeps[0], RENAME_SHARING_FIRST_BACKOFF_MS, "the held target cost a backoff");
  assert.deepEqual(readdirSync(directory), ["results.json"], "no temporary file is left behind");
});

test("save() starts no replace once its budget is spent, even when the holder lets go late", { skip: windowsSharing }, (t) => {
  const directory = mkdtempSync(join(testTempDirectory(), "launcher-save-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const target = join(directory, "results.json");
  writeFileSync(target, `${JSON.stringify({ state: "running" })}\n`);
  let held = openSync(target, "r");
  let clock = 0;
  const sleeps = [];
  try {
    assert.throws(
      () => save(target, { state: "passed" }, {
        now: () => clock,
        sleep: (milliseconds) => {
          // The thread wakes after the budget, and the holder has let go by then.
          sleeps.push(milliseconds);
          clock += RENAME_SHARING_BUDGET_MS + 1;
          closeSync(held);
          held = null;
        },
      }),
      (error) => error.code === "EPERM" && error.syscall === "rename",
    );
  } finally {
    if (held !== null) closeSync(held);
  }
  assert.equal(sleeps.length, 1);
  assert.deepEqual(json(target), { state: "running" }, "no replace started after the deadline");
  assert.deepEqual(readdirSync(directory), ["results.json"], "no temporary file is left behind");
});

test("save() keeps the previous results when the target stays held past its budget", { skip: windowsSharing }, (t) => {
  const directory = mkdtempSync(join(testTempDirectory(), "launcher-save-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const target = join(directory, "results.json");
  writeFileSync(target, `${JSON.stringify({ state: "running" })}\n`);
  const held = openSync(target, "r");
  let clock = 0;
  const sleeps = [];
  try {
    assert.throws(
      () => save(target, { state: "passed" }, {
        now: () => clock,
        sleep: (milliseconds) => {
          sleeps.push(milliseconds);
          clock += milliseconds;
        },
      }),
      (error) => error.code === "EPERM" && error.syscall === "rename",
    );
  } finally {
    closeSync(held);
  }
  assert.deepEqual(json(target), { state: "running" }, "the previous complete file stays");
  assert.deepEqual(readdirSync(directory), ["results.json"], "no temporary file is left behind");
  assert.equal(sleeps.reduce((total, milliseconds) => total + milliseconds, 0), RENAME_SHARING_BUDGET_MS);
  assert.equal(sleeps[0], RENAME_SHARING_FIRST_BACKOFF_MS);
  assert.ok(sleeps.every((milliseconds) => milliseconds <= RENAME_SHARING_MAX_BACKOFF_MS));
});

test("the atomic replace throws any other error at once, without waiting", (t) => {
  const directory = mkdtempSync(join(testTempDirectory(), "launcher-save-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const sleeps = [];
  assert.throws(
    () => replaceAtomically(join(directory, "missing.tmp"), join(directory, "results.json"), {
      platform: "win32",
      sleep: (milliseconds) => sleeps.push(milliseconds),
    }),
    (error) => error.code === "ENOENT",
  );
  assert.deepEqual(sleeps, []);
});

test("the atomic replace waits for a held target only on Windows", { skip: windowsSharing }, (t) => {
  const directory = mkdtempSync(join(testTempDirectory(), "launcher-save-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const target = join(directory, "results.json");
  writeFileSync(target, "{}\n");
  const temporary = `${target}.tmp`;
  writeFileSync(temporary, "{}\n");
  const held = openSync(target, "r");
  const sleeps = [];
  try {
    assert.throws(
      () => replaceAtomically(temporary, target, {
        platform: "linux",
        sleep: (milliseconds) => sleeps.push(milliseconds),
      }),
      (error) => error.code === "EPERM",
    );
  } finally {
    closeSync(held);
  }
  assert.deepEqual(sleeps, [], "another platform's error is not retried");
});

test("focused counts sum every libtest binary and ignore other output", () => {
  const output = [
    "   Compiling termal v0.1.0",
    "running 39 tests",
    "test result: ok. 39 passed; 0 failed; 0 ignored; 0 measured; 3863 filtered out; finished in 0.00s",
    "running 2 tests",
    "test result: FAILED. 1 passed; 1 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.01s",
    "test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s",
  ].join("\r\n");
  assert.deepEqual(focusedTestCounts(output), {
    runner: "cargo-libtest",
    passed: 44,
    failed: 1,
    ignored: 2,
  });
  // A runner whose result lines are not recognised records no counts, so its
  // focused run is never credited from them.
  assert.equal(focusedTestCounts("# pass 3\n# fail 0\nTests  3 passed (3)\n"), undefined);
  assert.equal(focusedTestCounts(""), undefined);
});

test("a focused run records its runner's counts and prints them with the input fingerprint", async (t) => {
  await repository(t, async (root) => {
    const output = [
      "test result: ok. 39 passed; 0 failed; 0 ignored; 0 measured; 3863 filtered out; finished in 0.00s",
      "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s",
    ].join("\n");
    const runDir = await createRun({
      root,
      stages: [stage("focused", `console.log(${JSON.stringify(output)})`)],
    }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.state, "passed");
    assert.deepEqual(json(join(runDir, "results.json")).stages[0].tests, {
      runner: "cargo-libtest",
      passed: 41,
      failed: 0,
      ignored: 1,
    });
    const summary = await summarize(runDir);
    assert.match(summary, /^tests: passed=41 failed=0 ignored=1$/mu);
    assert.match(summary, new RegExp(`^input fingerprint: ${result.after}$`, "mu"));
  });
});

test("a focused run without recognisable counts records none, and other stages never count", async (t) => {
  await repository(t, async (root) => {
    const runDir = await createRun({
      root,
      stages: [stage("focused", "console.log('all good')")],
    }, fixtureEnv);
    const result = await executeRun(runDir, fixtureEnv);
    assert.equal(result.state, "passed");
    assert.equal(json(join(runDir, "results.json")).stages[0].tests, undefined);
    assert.doesNotMatch(await summarize(runDir), /^tests: /mu);
  });
  await repository(t, async (root) => {
    const runDir = await createRun({
      root,
      stages: [stage("report", "console.log('test result: ok. 5 passed; 0 failed; 0 ignored;')")],
    }, fixtureEnv);
    await executeRun(runDir, fixtureEnv);
    assert.equal(json(join(runDir, "results.json")).stages[0].tests, undefined);
  });
});

test("focused counts read libtest lines a forced-colour runner wraps in terminal codes", () => {
  const output = [
    "test result: \u001b[32mok\u001b[0m. 39 passed; 0 failed; 0 ignored; 0 measured; 3863 filtered out; finished in 0.00s",
    "test result: \u001b[31mFAILED\u001b[0m. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s",
  ].join("\n");
  assert.deepEqual(focusedTestCounts(output), {
    runner: "cargo-libtest",
    passed: 40,
    failed: 1,
    ignored: 0,
  });
});

test("focused native Node counts require one coherent complete TAP or spec footer", () => {
  const footer = (prefix, passed = 1, failed = 0, skipped = 0, todo = 0) => [
    `${prefix} tests ${passed + failed + skipped + todo}`,
    `${prefix} suites 0`, `${prefix} pass ${passed}`, `${prefix} fail ${failed}`,
    `${prefix} cancelled 0`, `${prefix} skipped ${skipped}`, `${prefix} todo ${todo}`,
    `${prefix} duration_ms 1.5`,
  ].join("\n");
  for (const prefix of ["#", "ℹ"]) {
    const good = footer(prefix);
    assert.deepEqual(focusedTestCounts(good), { runner: "node-test", passed: 1, failed: 0, ignored: 0 });
    assert.deepEqual(focusedTestCounts(`${footer(prefix, 0, 1)}\n✖ failing tests:\nstack detail`),
      { runner: "node-test", passed: 0, failed: 1, ignored: 0 });
    assert.deepEqual(focusedTestCounts(footer(prefix, 0, 0, 1, 1)),
      { runner: "node-test", passed: 0, failed: 0, ignored: 2 });
    for (const invalid of [
      good.replace(`${prefix} tests 1`, `${prefix} tests 2`),
      good.replace(`${prefix} pass 1`, `${prefix} pass -1`),
      good.replace(`${prefix} pass 1`, `${prefix} pass 1.5`),
      good.replace(`${prefix} suites 0\n`, ""),
      good.replace(`${prefix} duration_ms 1.5`, ""),
      good.replace(`${prefix} pass 1`, `${prefix} pass 9007199254740992`),
      `${good}\n${good}`, `${good}\n${prefix} pass 1`,
      `${good}\ntest result: ok. 1 passed; 0 failed; 0 ignored;`,
    ]) assert.equal(focusedTestCounts(invalid), undefined, invalid);
  }
});

test("focused native Node launcher writes real TAP/spec success and failure counts", async (t) => {
  const directory = join(projectRoot, ".tmp");
  mkdirSync(directory, { recursive: true });
  for (const reporter of ["tap", "spec"]) {
    for (const failed of [false, true]) {
      await repository(t, async (root) => {
        writeFileSync(join(root, "fixture.test.mjs"),
          `import test from 'node:test'; test('selected case', () => {${failed ? "throw new Error('fixture failure');" : ""}});\n`);
        const env = { ...fixtureEnv };
        delete env.NODE_TEST_CONTEXT;
        const runDir = await createRun({ root, stages: [{ name: "focused", command: process.execPath,
          args: ["--test", `--test-reporter=${reporter}`, "--test-name-pattern", "selected case", "fixture.test.mjs"] }] }, env);
        const result = await executeRun(runDir, env);
        assert.equal(result.exitCode, failed ? 1 : 0);
        assert.equal(result.state, failed ? "failed" : "passed");
        assert.deepEqual(json(join(runDir, "results.json")).stages[0].tests,
          { runner: "node-test", passed: failed ? 0 : 1, failed: failed ? 1 : 0, ignored: 0 });
        assert.equal(result.before, result.after);
      }, fixtureEnv, directory);
    }
  }
});
