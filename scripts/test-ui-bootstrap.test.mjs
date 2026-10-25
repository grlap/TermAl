// Full-launcher setup regressions in disposable linked Git worktrees.
// A local npm CLI witness exercises the real owned command and stage boundary;
// these tests do not download packages, run Cargo or contact a live host.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync,
  symlinkSync, writeFileSync,
} from "node:fs";
import { dirname, isAbsolute, join, relative } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createRun, executeRun, recoverRun, summarize } from "./test-launcher.mjs";
import { runOwnedCommand } from "./test-process-owner.mjs";
import { bootstrapUiDependencies, resolveNpmCli, uiDependencyFiles } from "./test-ui-bootstrap.mjs";
import { isolatedGitEnvironment } from "./review-freeze-fingerprint.mjs";
import { removeFixtureOnPass } from "./test-fixture-cleanup.mjs";
import { withHostedRegistrationFixture } from "./test-launcher-registration-fixtures.mjs";
import { writeUiBootstrapFixture } from "./test-ui-bootstrap-fixtures.mjs";

const sourceRoot = realpathSync.native(fileURLToPath(new URL("../", import.meta.url)));
const json = (path) => JSON.parse(readFileSync(path, "utf8"));
const capacity = { statfs: () => ({ bavail: 200n * 1024n ** 3n, bsize: 1n }) };

function fixture(t) {
  const directory = join(sourceRoot, ".tmp", "ui-bootstrap-fixtures");
  mkdirSync(directory, { recursive: true });
  const canonical = realpathSync.native(directory);
  const fromRoot = relative(sourceRoot, canonical);
  assert.ok(fromRoot && !isAbsolute(fromRoot) && !fromRoot.split(/[\\/]/u).includes(".."));
  const base = mkdtempSync(join(canonical, "locked UI "));
  removeFixtureOnPass(t, base);
  const main = join(base, "main");
  mkdirSync(join(main, "ui"), { recursive: true });
  const env = isolatedGitEnvironment({
    ...process.env, TERMAL_BASE_URL: "http://127.0.0.1:0",
    TERMAL_SESSION_ID: "ui-bootstrap-fixture", TERMAL_CLI: process.execPath,
  });
  const git = (args, cwd = main) => execFileSync("git", args, {
    cwd, env, encoding: "utf8", windowsHide: true,
  });
  git(["init", "--quiet", "--template="]);
  for (const [key, value] of [
    ["user.name", "TermAl Test"], ["user.email", "test@example.invalid"],
    ["core.autocrlf", "false"], ["core.hooksPath", process.platform === "win32" ? "NUL" : "/dev/null"],
  ]) git(["config", key, value]);
  Object.assign(env, writeUiBootstrapFixture(main, join(main, ".git")));
  git(["add", "--", ".gitignore", "ui/package.json", "ui/package-lock.json"]);
  git(["commit", "--quiet", "--no-gpg-sign", "-m", "fixture"]);
  const root = join(base, "fresh candidate");
  git(["worktree", "add", "--quiet", "--detach", root, "HEAD"]);
  const admin = realpathSync.native(git(["rev-parse", "--absolute-git-dir"], root).trim());
  env.FIXTURE_UI_CWD = join(root, "ui");
  const events = env.FIXTURE_UI_EVENTS;
  const stages = uiDependencyFiles.map((name, index) => ({
    name: index === 0 ? "typescript" : "vitest", command: process.execPath,
    args: [join(root, "ui", name)], cwd: join(root, "ui"),
  }));
  assert.equal(existsSync(join(root, "ui/node_modules")), false);
  assert.equal(git(["status", "--porcelain"], root), "");
  return { root, main, base, admin, env, events, stages, git };
}

test("a fresh linked worktree bootstraps before readiness and either UI stage", async (t) => {
  const { root, env, events, stages, git } = fixture(t);
  await withHostedRegistrationFixture(root, env, async (hosted) => {
    const lock = readFileSync(join(root, "ui/package-lock.json"));
    const runDir = await createRun({ root, full: true, stages,
      prerequisiteFiles: uiDependencyFiles.map((name) => join(root, "ui", name)) }, hosted);
    let ready = 0;
    const result = await executeRun(runDir, hosted, { diskSpace: capacity, onReady: () => {
      ready += 1;
      const saved = json(join(runDir, "results.json"));
      assert.equal(saved.preflight.find(({ name }) => name === "ui-dependencies").state, "passed");
      assert.ok(saved.stages.every(({ state }) => state === "unrun"));
      assert.deepEqual(readFileSync(events, "utf8").trim().split("\n").map(JSON.parse), ["npm-ci"]);
    } });
    assert.equal(ready, 1);
    assert.equal(result.state, "passed");
    assert.equal(result.exitCode, 0);
    assert.deepEqual(result.stages.map(({ state }) => state), ["passed", "passed"]);
    assert.deepEqual(readFileSync(events, "utf8").trim().split("\n").map(JSON.parse), ["npm-ci", "tsc", "vitest.mjs"]);
    const setup = result.preflight.find(({ name }) => name === "ui-dependencies");
    assert.equal(setup.code, 0);
    assert.ok(setup.processOwner?.ledger, "the real setup uses owned process execution");
    assert.match(readFileSync(setup.log, "utf8"), /locked development dependencies/u);
    assert.deepEqual(readFileSync(join(root, "ui/package-lock.json")), lock);
    assert.equal(git(["status", "--porcelain"], root), "");
    assert.equal(result.before, result.after);
  });
});

test("npm setup failure is terminal before readiness with both UI stages unrun", async (t) => {
  const { root, env, events, stages } = fixture(t);
  await withHostedRegistrationFixture(root, { ...env, FIXTURE_UI_MODE: "fail" }, async (hosted) => {
    const runDir = await createRun({ root, full: true, stages }, hosted);
    let ready = false;
    await executeRun(runDir, hosted, { diskSpace: capacity, onReady: () => { ready = true; } });
    const saved = json(join(runDir, "results.json"));
    assert.equal(ready, false);
    assert.equal(saved.state, "failed");
    assert.equal(saved.exitCode, 1);
    assert.ok(saved.ended);
    assert.deepEqual(saved.stages.map(({ state }) => state), ["unrun", "unrun"]);
    assert.equal(saved.setupFailure.kind, "ui-dependencies");
    assert.equal(saved.setupFailure.phase, "npm-ci");
    const setup = saved.preflight.find(({ name }) => name === "ui-dependencies");
    assert.equal(setup.state, "failed");
    assert.equal(setup.code, 23);
    assert.match(setup.diagnostics.text, /fixture install error/u);
    assert.match(await summarize(runDir), /UI dependency setup failed \(npm-ci\)/u);
    assert.deepEqual(readFileSync(events, "utf8").trim().split("\n").map(JSON.parse), ["npm-ci"]);
  });
});

test("missing npm records its setup phase even though no process log exists", async (t) => {
  const { root, env, stages } = fixture(t);
  await withHostedRegistrationFixture(root, env, async (hosted) => {
    const runDir = await createRun({ root, full: true, stages }, hosted);
    let ready = false;
    await executeRun(runDir, hosted, { diskSpace: capacity, onReady: () => { ready = true; },
      uiSetup: { npmCli: () => { throw new Error("fixture npm unavailable"); } } });
    const saved = json(join(runDir, "results.json"));
    assert.equal(ready, false);
    assert.equal(saved.setupFailure.phase, "resolve-npm");
    assert.match(saved.error, /fixture npm unavailable/u);
    assert.equal(existsSync(saved.setupFailure.log), false);
    assert.ok(saved.stages.every(({ state }) => state === "unrun"));
  });
});

test("a missing TypeScript entrypoint becomes a durable setup failure before readiness", async (t) => {
  const { root, env, stages } = fixture(t);
  await withHostedRegistrationFixture(root, { ...env, FIXTURE_UI_MODE: "missing-entrypoints" }, async (hosted) => {
    const runDir = await createRun({ root, full: true, stages }, hosted);
    let ready = false;
    await executeRun(runDir, hosted, { diskSpace: capacity, onReady: () => { ready = true; } });
    const saved = json(join(runDir, "results.json"));
    assert.equal(ready, false);
    assert.equal(saved.state, "failed");
    assert.equal(saved.exitCode, 1);
    assert.ok(saved.ended);
    assert.equal(saved.setupFailure.kind, "ui-dependencies");
    assert.equal(saved.setupFailure.phase, "verify-entrypoints");
    assert.equal(saved.preflight.find(({ name }) => name === "ui-dependencies").code, 0);
    assert.match(saved.error, /UI dependency setup failed \(verify-entrypoints\): ENOENT/u);
    assert.ok(saved.error.includes(join("typescript", "bin", "tsc")));
    assert.deepEqual(saved.stages.map(({ state }) => state), ["unrun", "unrun"]);
    assert.match(await summarize(runDir), /UI dependency setup failed/u);
  });
});

test("focused execution does not install UI dependencies", async (t) => {
  const { root, env, events } = fixture(t);
  await withHostedRegistrationFixture(root, env, async (hosted) => {
    const runDir = await createRun({ root, full: false, scope: "focused", stages: [
      { name: "focused", command: process.execPath, args: ["-e", "console.log('focused fixture')"] },
    ] }, hosted);
    const result = await executeRun(runDir, hosted, { diskSpace: capacity,
      uiSetup: { npmCli: () => { throw new Error("must not bootstrap"); } } });
    assert.equal(result.state, "passed");
    assert.equal(existsSync(events), false);
    assert.equal(existsSync(join(root, "ui/node_modules")), false);
  });
});

test("bootstrap refuses an untracked lockfile before invoking npm", async (t) => {
  const { root, env, admin, git } = fixture(t);
  git(["rm", "--cached", "--", "ui/package-lock.json"], root);
  let ran = false;
  await assert.rejects(bootstrapUiDependencies(root, {
    env, log: join(admin, "setup.log"), run: () => { ran = true; },
  }), /validate-lockfile/u);
  assert.equal(ran, false);
});

test("bootstrap refuses a shared node_modules junction before invoking npm", async (t) => {
  const { root, main, env, admin } = fixture(t);
  const shared = join(main, "ui/node_modules");
  mkdirSync(shared);
  const marker = join(shared, "untouched");
  writeFileSync(marker, "private to another worktree");
  symlinkSync(shared, join(root, "ui/node_modules"), process.platform === "win32" ? "junction" : "dir");
  let ran = false;
  await assert.rejects(bootstrapUiDependencies(root, {
    env, log: join(admin, "setup.log"), run: () => { ran = true; },
  }), /cannot be a symlink or junction/u);
  assert.equal(ran, false);
  assert.equal(readFileSync(marker, "utf8"), "private to another worktree");
});

// This root's real dependency directory has an inbound junction. Reusing it
// must perform no npm operation, and a stale lock must leave its bytes intact.
for (const stale of [false, true]) {
  test(`an existing installation with an inbound link is ${stale ? "refused unchanged when stale" : "reused without npm when locked"}`, async (t) => {
    const { root, main, env, admin } = fixture(t);
    const ui = join(root, "ui"), modules = join(ui, "node_modules");
    const dependency = { version: "1.0.0", integrity: "fixture-integrity", dev: true };
    const lock = json(join(ui, "package-lock.json"));
    lock.packages["node_modules/witness"] = dependency;
    writeFileSync(join(ui, "package-lock.json"), JSON.stringify(lock));
    mkdirSync(join(modules, "witness"), { recursive: true });
    writeFileSync(join(modules, "witness/package.json"), JSON.stringify({ version: "1.0.0" }));
    const hidden = join(modules, ".package-lock.json");
    writeFileSync(hidden, JSON.stringify({ lockfileVersion: 3, packages: {
      "node_modules/witness": { ...dependency, version: stale ? "2.0.0" : "1.0.0" },
    } }));
    for (const name of uiDependencyFiles) {
      mkdirSync(dirname(join(ui, name)), { recursive: true });
      writeFileSync(join(ui, name), "must survive");
    }
    const inbound = join(main, "ui/node_modules");
    symlinkSync(modules, inbound, process.platform === "win32" ? "junction" : "dir");
    const bytes = readFileSync(hidden);
    let ran = false;
    const setup = bootstrapUiDependencies(root, {
      env, log: join(admin, "existing.log"),
      npmCli: () => { throw new Error("existing dependencies must not resolve npm"); },
      run: () => { ran = true; },
    });
    if (stale) await assert.rejects(setup, /verify-installed.*installed UI lock differs/u);
    else assert.equal((await setup).action, "reused");
    assert.equal(ran, false);
    assert.deepEqual(readFileSync(hidden), bytes);
    assert.equal(readFileSync(join(inbound, "typescript/bin/tsc"), "utf8"), "must survive");
  });
}

test("interrupting owned UI setup records interruption and leaves stages unrun", async (t) => {
  const { root, env, stages } = fixture(t);
  await withHostedRegistrationFixture(root, env, async (hosted) => {
    const runDir = await createRun({ root, full: true, stages }, hosted);
    const controller = new AbortController();
    let ready = false;
    const result = await executeRun(runDir, hosted, {
      diskSpace: capacity, signal: controller.signal, onReady: () => { ready = true; },
      uiSetup: { run: (command, args, options) => runOwnedCommand(command, args, {
        ...options, signal: controller.signal,
        onStart: () => controller.abort("fixture setup cancellation"),
      }) },
    });
    assert.equal(ready, false);
    assert.equal(result.state, "failed");
    assert.equal(result.interrupted, true);
    assert.match(result.error, /interrupted: fixture setup cancellation/u);
    assert.equal(result.setupFailure, undefined);
    const setup = result.preflight.find(({ name }) => name === "ui-dependencies");
    assert.equal(setup.interrupted, true);
    assert.equal(setup.cleanup.state, "reaped");
    assert.ok(result.stages.every(({ state }) => state === "unrun"));
  });
});

for (const matchingNonce of [false, true]) {
  test(`setup recovery waits for its owner then ${matchingNonce ? "imports exact cleanup" : "rejects a foreign cleanup ledger"}`, async (t) => {
    const { root, env, stages } = fixture(t);
    await withHostedRegistrationFixture(root, env, async (hosted) => {
      const runDir = await createRun({ root, full: true, stages, notifyTo: "fixture-coordinator" }, hosted);
      const resultPath = join(runDir, "results.json");
      const result = json(resultPath);
      const ledger = join(runDir, "setup-owner.json");
      result.pid = 4242;
      result.preflight = [{ name: "ui-dependencies", state: "running", phase: "npm-ci",
        processOwner: { pid: 4343, nonce: "setup-nonce", ledger } }];
      writeFileSync(resultPath, JSON.stringify(result));
      let notifications = 0;
      const send = async () => { notifications += 1; return { code: 0, signal: null }; };
      const bytes = readFileSync(resultPath);
      await assert.rejects(recoverRun(runDir, hosted, { send, isAlive: (pid) => pid === 4343 }), /process owner pid 4343/u);
      assert.deepEqual(readFileSync(resultPath), bytes);
      assert.equal(notifications, 0);
      const cleanup = { state: "reaped", trigger: "launcher-exit", pids: [], pidScope: "fixture" };
      writeFileSync(ledger, JSON.stringify({ nonce: matchingNonce ? "setup-nonce" : "another-run", cleanup }));
      await recoverRun(runDir, hosted, { send, isAlive: () => false });
      const recovered = json(resultPath);
      assert.equal(recovered.interrupted, true);
      assert.equal(recovered.preflight[0].state, "failed");
      assert.equal(recovered.preflight[0].interrupted, true);
      assert.ok(recovered.preflight[0].ended);
      assert.equal(recovered.preflight[0].cleanup.state, matchingNonce ? "reaped" : "unresolved");
      if (!matchingNonce) assert.match(recovered.preflight[0].cleanup.error, /nonce mismatch/u);
      assert.match(await summarize(runDir), /preflight ui-dependencies/u);
      assert.match(await summarize(runDir), /cleanup=(reaped|unresolved)/u);
      assert.ok(recovered.stages.every(({ state }) => state === "unrun"));
      assert.equal(notifications, 1);
      await recoverRun(runDir, hosted, { send, isAlive: () => false });
      assert.equal(notifications, 1);
    });
  });
}

for (const mode of ["missing-entrypoints", "changed-lockfile"]) {
  test(`successful npm exit cannot hide ${mode}`, async (t) => {
    const { root, env, admin } = fixture(t);
    await assert.rejects(bootstrapUiDependencies(root, {
      env, log: join(admin, "setup.log"), npmCli: () => "fixture-npm-cli.js",
      run: async () => {
        if (mode === "changed-lockfile") {
          for (const name of uiDependencyFiles) {
            const path = join(root, "ui", name);
            mkdirSync(dirname(path), { recursive: true });
            writeFileSync(path, "fixture");
          }
          writeFileSync(join(root, "ui/package-lock.json"), "changed");
        }
        return { code: 0 };
      },
    }), (error) => {
      assert.equal(error.setupFailure.phase, "verify-entrypoints");
      if (mode === "changed-lockfile") assert.match(error.message, /changed the candidate lockfile/u);
      return true;
    });
  });
}

test("npm resolution uses standard Node installation layouts without shell shims", (t) => {
  const { base } = fixture(t);
  const node = join(base, "toolchain", process.platform === "win32" ? "node.exe" : "node");
  const cli = join(dirname(node), "node_modules/npm/bin/npm-cli.js");
  mkdirSync(dirname(cli), { recursive: true });
  writeFileSync(node, "fixture executable");
  writeFileSync(cli, "fixture npm");
  assert.equal(resolveNpmCli({ PATH: "" }, node), realpathSync(cli));
  const missing = join(base, "without npm", "node");
  mkdirSync(dirname(missing));
  writeFileSync(missing, "fixture executable without npm");
  assert.throws(() => resolveNpmCli({ PATH: "" }, missing), /npm CLI is unavailable/u);
});

test("npm resolution supports lib layouts and skips non-npm inherited entrypoints", (t) => {
  const { base } = fixture(t);
  const node = join(base, "prefix/bin/node");
  const cli = join(base, "prefix/lib/node_modules/npm/bin/npm-cli.js");
  mkdirSync(dirname(node), { recursive: true });
  mkdirSync(dirname(cli), { recursive: true });
  writeFileSync(node, "fixture node");
  writeFileSync(cli, "fixture npm");
  const pnpm = join(base, "pnpm.cjs");
  writeFileSync(pnpm, "another package manager");
  assert.equal(resolveNpmCli({ PATH: "", npm_execpath: pnpm }, node), realpathSync(cli));
  const inherited = join(base, "inherited/npm-cli.js");
  mkdirSync(dirname(inherited));
  writeFileSync(inherited, "inherited npm");
  assert.equal(resolveNpmCli({ PATH: "", npm_execpath: inherited }, node), realpathSync(inherited));
});

test("npm resolution follows a POSIX PATH npm symlink", { skip: process.platform === "win32" }, (t) => {
  const { base } = fixture(t);
  const node = join(base, "node-only/node");
  const cli = join(base, "separate/npm-cli.js"), bin = join(base, "path-bin");
  mkdirSync(dirname(node), { recursive: true });
  mkdirSync(dirname(cli), { recursive: true });
  mkdirSync(bin);
  writeFileSync(node, "fixture node");
  writeFileSync(cli, "fixture npm");
  symlinkSync(cli, join(bin, "npm"));
  assert.equal(resolveNpmCli({ PATH: bin }, node), realpathSync(cli));
});
