// Run-directory compatibility at the real launcher/registration boundary.
// Git replies are changed only in an isolated Node child; the local service
// and disposable repositories remain real. No product or Cargo test is run.
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import {
  existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync,
  realpathSync, writeFileSync,
} from "node:fs";
import { isAbsolute, join, relative } from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { isolatedGitEnvironment } from "./review-freeze-fingerprint.mjs";
import { HELPER_TEST_HANG_GUARD_MS } from "./test-launcher.mjs";
import { registrationHost } from "./test-launcher-registration-fixtures.mjs";
import { removeFixtureOnPass } from "./test-fixture-cleanup.mjs";

const launcherUrl = new URL("./test-launcher.mjs", import.meta.url).href;
const sourceRoot = realpathSync.native(fileURLToPath(new URL("../", import.meta.url)));

function fixture(t, linked, longRun = false) {
  const directory = join(sourceRoot, ".tmp", "launcher-git-path-fixtures");
  mkdirSync(directory, { recursive: true });
  const canonicalDirectory = realpathSync.native(directory);
  const fromSource = relative(sourceRoot, canonicalDirectory);
  assert.ok(fromSource && !isAbsolute(fromSource) && !fromSource.split(/[\\/]/u).includes(".."));
  const base = mkdtempSync(join(canonicalDirectory, "launcher git path "));
  removeFixtureOnPass(t, base);
  let worktreeBase = base;
  if (longRun) {
    // Git refuses an explicit GIT_DIR above PATH_MAX - 40 even with
    // core.longpaths. Leave its admin path at 210; review-runs and the run
    // id then put the real run directory beyond 260 and its bootstrap past 300.
    const padding = 210 - join(base, "main worktree", ".git", "worktrees", "linked-worktree").length - 1;
    assert.ok(padding > 0, "fixture checkout must leave room below Git's own GIT_DIR limit");
    worktreeBase = join(base, "x".repeat(padding));
  }
  mkdirSync(worktreeBase, { recursive: true });
  const main = join(worktreeBase, "main worktree");
  mkdirSync(main);
  const env = isolatedGitEnvironment({
    ...process.env,
    TERMAL_BASE_URL: "http://127.0.0.1:0",
    TERMAL_SESSION_ID: "git-path-fixture-owner",
    TERMAL_CLI: process.execPath,
  });
  delete env.NODE_TEST_CONTEXT;
  const git = (args, cwd = main) => execFileSync("git", args, {
    cwd, env, encoding: "utf8", windowsHide: true,
  });
  git(["init", "--quiet", "--template="]);
  for (const [key, value] of [
    ["user.name", "TermAl Test"], ["user.email", "test@example.invalid"],
    ["core.autocrlf", "false"],
    ["core.hooksPath", process.platform === "win32" ? "NUL" : "/dev/null"],
  ]) git(["config", key, value]);
  writeFileSync(join(main, "tracked.txt"), "fixture\n");
  git(["add", "tracked.txt"]);
  git(["commit", "--quiet", "--no-gpg-sign", "-m", "fixture"]);
  const root = linked ? join(worktreeBase, "linked worktree") : main;
  if (linked) git(["worktree", "add", "--quiet", "--detach", root, "HEAD"]);
  const admin = realpathSync.native(git(["rev-parse", "--absolute-git-dir"], root).replace(/\r?\n$/u, ""));
  if (linked) assert.notEqual(admin, realpathSync.native(join(main, ".git")));
  return { base, root, admin, env };
}

// Include Git metadata in the no-side-effects witness. Only the explicitly
// expected shim trace is excluded; a malformed run directory is never hidden.
function pathsBelow(root, ignored) {
  const paths = [];
  const visit = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (path === ignored) continue;
      paths.push(relative(root, path));
      if (entry.isDirectory()) visit(path);
    }
  };
  visit(root);
  return paths.sort();
}

function writeShim(admin, config) {
  const preload = join(admin, "git-path-shim.mjs");
  const trace = join(admin, "git-path-queries.jsonl");
  // Legacy mode simulates only the unsupported --path-format reply. Other
  // queries use the installed Git; this is not a complete Git 2.29 emulator.
  // The fixture intercepts spawnSync rather than installing a .cmd shim in
  // PATH: native .cmd execution differs between Windows and POSIX. Importing
  // this before the launcher and syncing builtins changes its actual binding.
  writeFileSync(preload, [
    "import childProcess from 'node:child_process';",
    "import { appendFileSync } from 'node:fs';",
    "import { syncBuiltinESMExports } from 'node:module';",
    `const config = ${JSON.stringify(config)};`,
    `const trace = ${JSON.stringify(trace)};`,
    "const original = childProcess.spawnSync;",
    "childProcess.spawnSync = (command, args, options) => {",
    "  const git = command === 'git';",
    "  if (git && args[0] === '--version' && config.mode === 'legacy') {",
    "    return { status: 0, signal: null, stdout: 'git version 2.29.2\\n', stderr: '' };",
    "  }",
    "  const oldPathQuery = git && args[0] === 'rev-parse' && args.includes('--git-path') && args.includes('review-runs');",
    "  const absoluteQuery = git && args[0] === 'rev-parse' && args.includes('--absolute-git-dir');",
    "  if (oldPathQuery || absoluteQuery) {",
    "    appendFileSync(trace, JSON.stringify(args) + '\\n');",
    "    if (config.mode === 'malformed') return { status: 0, signal: null, stdout: config.stdout, stderr: '' };",
    "    if (config.mode === 'legacy' && args.includes('--path-format=absolute')) {",
    "      const supported = original(command, args.filter(arg => arg !== '--path-format=absolute'), options);",
    "      return { ...supported, stdout: '--path-format=absolute\\n' + supported.stdout };",
    "    }",
    "  }",
    "  return original(command, args, options);",
    "};",
    "syncBuiltinESMExports();",
  ].join("\n"));
  return { preload, trace };
}

async function launch(root, admin, env, preload, execute) {
  const source = [
    "import { spawnSync } from 'node:child_process';",
    "import { existsSync, readFileSync } from 'node:fs';",
    "import { join } from 'node:path';",
    `import { createRun, executeRun } from ${JSON.stringify(launcherUrl)};`,
    `const marker = ${JSON.stringify(join(admin, "stage-executed"))};`,
    "let runDir; let result; let error;",
    "try {",
    `  runDir = await createRun({ root: ${JSON.stringify(root)}, full: false, scope: 'focused',`,
    "    stages: [{ name: 'focused', command: process.execPath, args: ['-e',",
    `      ${JSON.stringify(`require('node:fs').appendFileSync(${JSON.stringify(join(admin, "stage-executed"))}, 'x')`)}] }],`,
    "  }, process.env);",
    execute ? "  result = await executeRun(runDir, process.env);" : "",
    "} catch (failure) { error = failure.message; }",
    "console.log('GIT_PATH_FIXTURE_RESULT ' + JSON.stringify({ runDir, error, state: result?.state,",
    "  executed: existsSync(marker) ? readFileSync(marker, 'utf8') : '',",
    "  gitVersion: spawnSync('git', ['--version'], { encoding: 'utf8', windowsHide: true }).stdout.trim(),",
    "}));",
  ].join("\n");
  const child = spawn(process.execPath, [
    "--import", pathToFileURL(preload).href, "--input-type=module", "--eval", source,
  ], { cwd: root, env, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (chunk) => { stdout += chunk; });
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => code === 0 && !signal
      ? resolve() : reject(new Error(`fixture child exit ${code}/${signal}: ${stderr}\n${stdout}`)));
  });
  const reports = stdout.split(/\r?\n/u).filter((line) => line.startsWith("GIT_PATH_FIXTURE_RESULT "));
  assert.equal(reports.length, 1, `${stdout}\n${stderr}`);
  return JSON.parse(reports[0].slice("GIT_PATH_FIXTURE_RESULT ".length));
}

const layouts = [{ linked: false, longRun: false }, { linked: true, longRun: false }];
if (process.platform === "win32") layouts.push({ linked: true, longRun: true });
for (const { linked, longRun } of layouts) {
  for (const mode of ["current", "legacy"]) {
    test(`launcher Git path ${mode}: ${linked ? "linked" : "main"} worktree with spaces${longRun ? " beyond MAX_PATH" : ""}`,
      { timeout: HELPER_TEST_HANG_GUARD_MS }, async (t) => {
        const { root, admin, env } = fixture(t, linked, longRun);
        const { preload, trace } = writeShim(admin, { mode });
        await registrationHost(async (host) => {
          const result = await launch(root, admin, { ...env, TERMAL_BASE_URL: host.url }, preload, true);
          if (mode === "legacy") assert.equal(result.gitVersion, "git version 2.29.2");
          assert.equal(result.error, undefined, JSON.stringify(result));
          assert.ok(isAbsolute(result.runDir));
          assert.match(relative(realpathSync.native(join(admin, "review-runs")), realpathSync.native(result.runDir)), /^test-[^\\/]+$/u);
          assert.equal(result.state, "passed", JSON.stringify(result));
          assert.equal(result.executed, "x", "exactly one stage executes after registration");
          if (process.platform === "win32") {
            if (longRun) {
              assert.ok(result.runDir.length > 260, "keep the actual run directory beyond MAX_PATH");
              assert.ok(join(result.runDir, "focused.log.owner-request.json.bootstrap.jsonl").length > 300,
                "exercise the old framework bootstrap write well beyond 260 characters");
            }
            // Linked Git metadata deliberately makes run-local compiler TEMP
            // too deep on the failing input. Keep that real run path intact.
            const phases = readFileSync(join(result.runDir, "focused.log.owner-request.json.bootstrap.jsonl"), "utf8")
              .trim().split(/\r?\n/u).map((line) => JSON.parse(line));
            const expectedPhases = ["script-entered", "request-read", "before-add-type", "after-add-type",
              "native-entered", "ledger-open", "ledger-committed", "native-returned", "powershell-close"];
            let previousPhase = -1;
            for (const phase of expectedPhases) {
              const index = phases.findIndex((entry) => entry.phase === phase);
              assert.ok(index > previousPhase, `preserve every bootstrap phase in order: ${phase}`);
              previousPhase = index;
            }
            const scratch = phases.find((entry) => entry.phase === "before-add-type").compilerScratch;
            assert.ok(isAbsolute(scratch));
            assert.ok(scratch.length <= 200, "leave room for native compiler temporary names");
            assert.match(relative(join(sourceRoot, ".tmp", "ow"), scratch), /^o-[A-Za-z0-9]{6}$/u);
            assert.ok(result.runDir.length > scratch.length, "linked/main metadata remains deeper than compiler scratch");
            const retained = phases.find((entry) => entry.phase === "compiler-scratch-retained");
            const cleanupDiagnostic = JSON.stringify({ retained,
              remaining: existsSync(scratch) ? readdirSync(scratch) : [],
              runPathLength: result.runDir.length, scratchLength: scratch.length });
            assert.equal(existsSync(scratch), false, `guardian removes compiler scratch after PowerShell exits: ${cleanupDiagnostic}`);
            assert.ok(phases.some((entry) => entry.phase === "compiler-scratch-removed" && entry.compilerScratch === scratch));
          }
          assert.equal(host.requests.length, 1);
          assert.equal(host.queries.length, 2, "parent and executor each confirm the registration");
          assert.equal(realpathSync.native(host.requests[0].body.runDirectory), realpathSync.native(result.runDir));
          assert.ok(readFileSync(trace, "utf8").length > 0, "the launcher must really query the fixture Git");
        }, { autoAcknowledge: true });
      });
  }
}

for (const [name, reply] of [
  ["empty", () => "\n"],
  ["relative", () => "relative-run-directory\n"],
  ["multiple lines", (admin) => `${join(admin, "first")}\n${join(admin, "second")}\n`],
  ["echoed option", () => "--path-format=absolute\n"],
  ["NUL", (admin) => `${join(admin, "invalid\u0000path")}\n`],
  ["control", (admin) => `${join(admin, "invalid\u0007path")}\n`],
]) {
  test(`launcher Git path refuses ${name} before mkdir or registration`,
    { timeout: HELPER_TEST_HANG_GUARD_MS }, async (t) => {
      const { base, root, admin, env } = fixture(t, false);
      const { preload, trace } = writeShim(admin, { mode: "malformed", stdout: reply(admin) });
      const before = pathsBelow(base, trace);
      await registrationHost(async (host) => {
        const result = await launch(root, admin, { ...env, TERMAL_BASE_URL: host.url }, preload, false);
        assert.match(result.error ?? "", /cannot locate Git run directory/iu, JSON.stringify(result));
        assert.equal(result.runDir, undefined);
        assert.equal(result.executed, "");
        assert.equal(host.requests.length, 0, "invalid Git output must never be registered");
        assert.equal(host.queries.length, 0);
        assert.deepEqual(pathsBelow(base, trace), before, "invalid Git output must not create any run directory or artifact");
      }, { autoAcknowledge: true });
    });
}
