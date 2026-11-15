import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { join, sep } from "node:path";
import test from "node:test";
import { runOwnedCommand } from "./test-process-owner.mjs";
import { removeFixtureOnPass } from "./test-fixture-cleanup.mjs";
import {
  PARALLEL_FULL_SCHEDULE, assertParallelHostObservation, fullSchedule,
  isolatedStageEnvironments, parallelMachineHash, runStageSchedule, validateCargoOutputTarget,
} from "./test-full-schedule.mjs";

const names = ["fmt", "cargo-check", "typescript", "fingerprint-tests", "rust-tests", "vitest"];
const stages = () => names.map((name) => ({ name,
  ...(["rust-tests", "vitest"].includes(name) ? { schedule: PARALLEL_FULL_SCHEDULE } : {}) }));
const deferred = () => { let resolve; const promise = new Promise((done) => { resolve = done; }); return { promise, resolve }; };

test("Cargo output validation reads ancestors without creating directories", (t) => {
  const parent = join(import.meta.dirname, "..", ".tmp");
  mkdirSync(parent, { recursive: true });
  const root = mkdtempSync(join(parent, "full-target-check-"));
  removeFixtureOnPass(t, root);
  assert.equal(validateCargoOutputTarget(root, "missing/output"), join(root, "missing", "output"));
  assert.throws(() => readFileSync(join(root, "missing")), /ENOENT/);
  assert.throws(() => validateCargoOutputTarget(root, `..${sep}outside`), /escapes|inside/);
  assert.throws(() => validateCargoOutputTarget(root, root), /inside|root/);
});

test("Cargo output validation refuses a link before lexical dot-dot normalization", (t) => {
  const parent = join(import.meta.dirname, "..", ".tmp");
  mkdirSync(parent, { recursive: true });
  const root = mkdtempSync(join(parent, "full-target-link-"));
  removeFixtureOnPass(t, root);
  mkdirSync(join(root, "plain"));
  symlinkSync(join(root, "plain"), join(root, "linked"), process.platform === "win32" ? "junction" : "dir");
  for (const target of [join(root, "linked", "missing"), `${root}${sep}linked${sep}..${sep}output`]) {
    assert.throws(() => validateCargoOutputTarget(root, target), /plain directory/);
  }
  assert.throws(() => readFileSync(join(root, "output")), /ENOENT/);
});

test("parallel host admission requires a native Linux observation and an exact allowed machine id", () => {
  const id = "ab".repeat(16);
  const hash = parallelMachineHash(id);
  assert.match(hash, /^[a-f0-9]{64}$/);
  assert.notEqual(hash, id);
  assert.doesNotThrow(() => assertParallelHostObservation("linux", `${id}\n`, [hash]));
  for (const [platform, observed, allowed] of [
    ["win32", id, [hash]], ["darwin", id, [hash]], ["linux", "cd".repeat(16), [hash]],
    ["linux", id, []], ["linux", `${id}extra`, [hash]], ["linux", "", [hash]],
  ]) assert.throws(() => assertParallelHostObservation(platform, observed, allowed), /DL560/);
});

test("a scheduling marker cannot turn a focused or partial stage selection into the full experiment", () => {
  assert.equal(fullSchedule(names.map((name) => ({ name })), true), undefined);
  assert.equal(fullSchedule(stages(), true), PARALLEL_FULL_SCHEDULE);
  assert.throws(() => fullSchedule(stages(), false), /full/);
  assert.throws(() => fullSchedule(stages().slice(1), true), /stage/);
  assert.throws(() => fullSchedule(stages().map((s) => s.name === "fmt" ? { ...s, schedule: PARALLEL_FULL_SCHEDULE } : s), true), /stage/);
  assert.throws(() => fullSchedule(stages().map((s) => s.name === "vitest" ? { name: s.name } : s), true), /stage/);
  assert.throws(() => fullSchedule([{ name: "focused", schedule: "other" }], false), /schedule/);
});

test("the entire Node predecessor settles before either sibling starts, then both overlap", async () => {
  const helper = deferred(), siblings = [deferred(), deferred()], bothStarted = deferred();
  const seen = [];
  const execution = runStageSchedule(names.length, async (index) => {
    seen.push(`start:${index}`);
    if (index === 3) await helper.promise;
    if (index >= 4) {
      if (seen.includes("start:4") && seen.includes("start:5")) bothStarted.resolve();
      await siblings[index - 4].promise;
    }
    seen.push(`end:${index}`);
    return true;
  }, { parallel: true });
  // Event barriers, not timing assertions: neither final stage can run while
  // the deliberately held Node helper is still executing.
  await new Promise((done) => setImmediate(done));
  assert.deepEqual(seen, ["start:0", "end:0", "start:1", "end:1", "start:2", "end:2", "start:3"]);
  helper.resolve();
  await bothStarted.promise;
  assert.ok(seen.indexOf("end:3") < seen.indexOf("start:4"));
  assert.ok(seen.indexOf("end:3") < seen.indexOf("start:5"));
  siblings[1].resolve(); siblings[0].resolve();
  await execution;
  assert.equal(seen.filter((x) => x.startsWith("end:")).length, names.length);
});

test("default execution stays sequential and fails fast; opt-in retains ordinary failures from all stages", async () => {
  const sequential = [];
  await runStageSchedule(6, async (i) => { sequential.push(i); return i !== 1; });
  assert.deepEqual(sequential, [0, 1]);
  const complete = [];
  await runStageSchedule(6, async (i) => { complete.push(i); return false; }, { parallel: true });
  assert.deepEqual(complete, [0, 1, 2, 3, 4, 5]);
});

test("a fatal sibling error fans out cancellation and waits for sibling settlement before rejecting", async () => {
  const failure = new Error("result publication failed"), release = deferred(), started = deferred();
  let settled = false, fatal;
  const execution = runStageSchedule(6, async (i) => {
    if (i === 4) { await started.promise; throw failure; }
    if (i === 5) { started.resolve(); await release.promise; settled = true; }
    return true;
  }, { parallel: true, onFatal: (error) => { fatal = error; release.resolve(); } });
  await assert.rejects(execution, (error) => error === failure);
  assert.equal(fatal, failure);
  assert.equal(settled, true);
});

test("stage homes, Engram stores, Git config and temp trees are private without changing parent toolchain context", () => {
  const parent = join(import.meta.dirname, "..", ".tmp");
  mkdirSync(parent, { recursive: true });
  const root = mkdtempSync(join(parent, "full-schedule-"));
  try {
    const env = { HOME: root, CARGO_HOME: join(root, "cargo"), RUSTUP_HOME: join(root, "rustup"),
      CARGO_TARGET_DIR: join(root, "target"), TERMAL_TEST_RUN_ROOT: "/unrelated/run-old",
      GIT_DIR: "/live/git", ENGRAM_HOME: "/live/engram", TERMAL_TEST_THREADS: "4", CARGO_BUILD_JOBS: "48" };
    const original = { ...env };
    const isolated = isolatedStageEnvironments(root, "test-environments", env);
    const rust = isolated.environments["rust-tests"], ui = isolated.environments.vitest;
    assert.deepEqual(env, original);
    for (const key of ["HOME", "ENGRAM_HOME", "TMPDIR", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "NODE_COMPILE_CACHE"]) {
      assert.notEqual(rust[key], ui[key], key);
      assert.ok(rust[key].startsWith(root), key);
      assert.ok(ui[key].startsWith(root), key);
    }
    writeFileSync(join(rust.ENGRAM_HOME, "fixture-store"), "rust only");
    assert.throws(() => readFileSync(join(ui.ENGRAM_HOME, "fixture-store")), /ENOENT/);
    for (const child of [rust, ui]) {
      assert.equal(child.GIT_DIR, undefined);
      assert.equal(child.TERMAL_TEST_RUN_ROOT, undefined);
      assert.equal(child.GIT_CONFIG_NOSYSTEM, "1");
      assert.equal(child.CARGO_TARGET_DIR, env.CARGO_TARGET_DIR);
      assert.equal(child.CARGO_HOME, env.CARGO_HOME);
      assert.equal(child.RUSTUP_HOME, env.RUSTUP_HOME);
      assert.equal(child.TERMAL_TEST_THREADS, "4");
      assert.equal(child.CARGO_BUILD_JOBS, "48");
      assert.equal(child.TMPDIR, child.TERMAL_TEST_USER_TEMP);
    }
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("stage environment creation rejects a linked temporary parent", { skip: process.platform === "win32" }, () => {
  const parent = join(import.meta.dirname, "..", ".tmp");
  mkdirSync(parent, { recursive: true });
  const root = mkdtempSync(join(parent, "full-schedule-link-"));
  try {
    mkdirSync(join(root, "other"));
    symlinkSync(join(root, "other"), join(root, ".tmp"));
    assert.throws(() => isolatedStageEnvironments(root, "test-link", { HOME: root }), /plain directory/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("native sibling owners overlap in private homes and retain an ordinary failure without cancelling the other", async (t) => {
  const parent = join(import.meta.dirname, "..", ".tmp");
  mkdirSync(parent, { recursive: true });
  const root = mkdtempSync(join(parent, "full-native-pair-"));
  removeFixtureOnPass(t, root);
  const fixtureEnv = { ...process.env, HOME: root };
  for (const key of ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "CARGO_BUILD_BUILD_DIR"]) {
    delete fixtureEnv[key];
  }
  const isolation = isolatedStageEnvironments(root, "test-native-pair", fixtureEnv);
  const outcomes = [];
  const source = `
    const fs = require('node:fs'), path = require('node:path');
    const [directory, own, peer, code] = process.argv.slice(1);
    console.log(JSON.stringify({home:process.env.HOME, engram:process.env.ENGRAM_HOME}));
    fs.writeFileSync(path.join(directory, own), 'ready');
    let watcher;
    const finish = () => {
      if (!fs.existsSync(path.join(directory, peer))) return;
      watcher?.close(); process.exit(Number(code));
    };
    watcher = fs.watch(directory, finish);
    finish();
  `;
  await runStageSchedule(6, async (index) => {
    if (index < 4) return true;
    const name = names[index], other = names[index === 4 ? 5 : 4];
    outcomes[index - 4] = await runOwnedCommand(process.execPath,
      ["-e", source, root, `${name}.ready`, `${other}.ready`, index === 4 ? "7" : "0"], {
        cwd: root, env: isolation.environments[name], log: join(root, `${name}.log`),
      });
    return outcomes[index - 4].code === 0;
  }, { parallel: true });
  assert.deepEqual(outcomes.map((result) => result.code), [7, 0]);
  for (let i = 0; i < 2; i += 1) {
    assert.equal(outcomes[i].cleanup.state, "reaped");
    const observed = JSON.parse(readFileSync(join(root, `${names[i + 4]}.log`), "utf8"));
    assert.equal(observed.home, isolation.environments[names[i + 4]].HOME);
    assert.equal(observed.engram, isolation.environments[names[i + 4]].ENGRAM_HOME);
  }
});

test("one interruption settles both native process groups", async (t) => {
  const parent = join(import.meta.dirname, "..", ".tmp");
  mkdirSync(parent, { recursive: true });
  const root = mkdtempSync(join(parent, "full-native-cancel-"));
  removeFixtureOnPass(t, root);
  const controller = new AbortController();
  let started = 0;
  const results = await Promise.all([0, 1].map((i) => runOwnedCommand(process.execPath,
    ["-e", "setInterval(() => {}, 1000)"], {
      cwd: root, env: process.env, log: join(root, `stage-${i}.log`), signal: controller.signal,
      onStart: () => { if (++started === 2) controller.abort("fixture interruption"); },
    })));
  assert.equal(started, 2);
  for (const result of results) {
    assert.equal(result.interrupted, true);
    assert.equal(result.code, 1);
    assert.equal(result.cleanup.state, "reaped");
  }
});
