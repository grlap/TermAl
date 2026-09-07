// Owns regression coverage for test-run containment and cleanup. Does not run
// Cargo or touch the user's historical temporary files.
import assert from "node:assert/strict";
import fs from "node:fs";
import { mkdirSync, mkdtempSync, existsSync, readdirSync, rmSync, writeFileSync, utimesSync, symlinkSync } from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { runInTestTemp, sweepStaleTestRuns } from "./test-temp-root.mjs";

function sandbox(t) {
  const parent = join(tmpdir(), "termal", "tests");
  mkdirSync(parent, { recursive: true });
  const root = mkdtempSync(join(parent, "containment-contract-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}

function child(userTemp, source) {
  return runInTestTemp(process.execPath, ["-e", source], {
    userTemp,
    report: () => {},
  });
}

test("green children receive one contained root which is removed after exit", async (t) => {
  const userTemp = sandbox(t);
  const result = await child(userTemp, `
    const fs = require('node:fs');
    const path = require('node:path');
    const assert = require('node:assert/strict');
    assert.equal(process.env.TEMP, process.env.TMP);
    assert.equal(process.env.TMP, process.env.TMPDIR);
    assert.equal(process.env.TEMP, process.env.TERMAL_TEST_RUN_ROOT);
    assert.equal(path.dirname(process.env.TEMP), path.join(process.env.TERMAL_TEST_USER_TEMP, 'termal', 'tests'));
    fs.mkdirSync(path.join(process.env.TEMP, 'termal-test-state-child'));
  `);
  assert.equal(result.exitCode, 0);
  assert.equal(result.beforeCount, 0);
  assert.equal(result.afterCount, 0);
  assert.equal(existsSync(result.runRoot), false);
});

test("failed runs retain evidence and preserve the child's exit code", async (t) => {
  const result = await child(sandbox(t), "process.exit(23)");
  assert.equal(result.exitCode, 23);
  assert.equal(existsSync(result.runRoot), true);
});

test("a new root-level artifact fails a green child without deleting the artifact", async (t) => {
  const userTemp = sandbox(t);
  const result = await child(userTemp, `
    require('node:fs').writeFileSync(require('node:path').join(process.env.TERMAL_TEST_USER_TEMP, 'termal-escaped'), 'evidence');
  `);
  assert.notEqual(result.exitCode, 0);
  assert.equal(result.afterCount - result.beforeCount, 1);
  assert.equal(existsSync(join(userTemp, "termal-escaped")), true);
});

test("existing root-level entries are counted, not deleted or blamed on this run", async (t) => {
  const userTemp = sandbox(t);
  mkdirSync(join(userTemp, "termal-existing"));
  const result = await child(userTemp, "");
  assert.equal(result.exitCode, 0);
  assert.equal(result.beforeCount, 1);
  assert.equal(result.afterCount, 1);
  assert.equal(existsSync(join(userTemp, "termal-existing")), true);
});

test("a same-count replacement still fails containment and preserves evidence", async (t) => {
  const userTemp = sandbox(t);
  writeFileSync(join(userTemp, "termal-old"), "old");
  const result = await child(userTemp, `
    const fs = require('node:fs');
    const path = require('node:path');
    fs.unlinkSync(path.join(process.env.TERMAL_TEST_USER_TEMP, 'termal-old'));
    fs.writeFileSync(path.join(process.env.TERMAL_TEST_USER_TEMP, 'termal-new'), 'new');
  `);
  assert.equal(result.beforeCount, result.afterCount);
  assert.notEqual(result.exitCode, 0);
  assert(existsSync(join(userTemp, "termal-new")));
  assert(existsSync(result.runRoot));
});

test("unrecognized stale marker shapes never block a child or get deleted", async (t) => {
  const userTemp = sandbox(t);
  const root = join(userTemp, "termal", "tests");
  mkdirSync(root, { recursive: true });
  const invalid = [null, 7, "text", [], { version: 2, pid: 2147483647 },
    { version: 1, pid: "2147483647" }, { version: 1, pid: 2147483647, childPid: 0 }];
  for (const [index, value] of invalid.entries()) {
    const path = join(root, `run-invalid-${index}`);
    mkdirSync(path);
    const marker = join(path, ".termal-test-run");
    writeFileSync(marker, JSON.stringify(value));
    utimesSync(marker, new Date(0), new Date(0));
  }
  const result = await child(userTemp, "");
  assert.equal(result.exitCode, 0);
  assert.equal(readdirSync(root).length, invalid.length);
});

test("manual run roots use the same direct run-* and no-traversal contract as Rust", (t) => {
  const userTemp = sandbox(t);
  const root = join(userTemp, "termal", "tests");
  const moduleUrl = new URL("./test-temp-root.mjs", import.meta.url).href;
  for (const runRoot of ["", join(root, "arbitrary"), `${root}/run-child/../run-other`]) {
    const result = spawnSync(process.execPath, ["--input-type=module", "-e", `
      import { testTempDirectory } from ${JSON.stringify(moduleUrl)};
      testTempDirectory();
    `], {
      env: { ...process.env, TERMAL_TEST_USER_TEMP: userTemp, TERMAL_TEST_RUN_ROOT: runRoot },
      encoding: "utf8", windowsHide: true,
    });
    assert.equal(result.error, undefined);
    assert.notEqual(result.status, 0, `accepted invalid root ${runRoot}`);
    assert.match(result.stderr, /direct run-\* child|parent traversal/);
  }
});

test("manual user-temp aliases stay lexical while parent traversal is rejected", (t) => {
  const userTemp = sandbox(t);
  const alias = join(userTemp, "alias");
  const actual = join(userTemp, "actual");
  mkdirSync(actual);
  symlinkSync(actual, alias, process.platform === "win32" ? "junction" : "dir");
  const moduleUrl = new URL("./test-temp-root.mjs", import.meta.url).href;
  for (const temp of [alias, `${actual}/../actual`, ""]) {
    const env = { ...process.env, TERMAL_TEST_USER_TEMP: temp };
    delete env.TERMAL_TEST_RUN_ROOT;
    const result = spawnSync(process.execPath, ["--input-type=module", "-e", `
      import { testTempDirectory } from ${JSON.stringify(moduleUrl)};
      console.log(testTempDirectory());
    `], { env, encoding: "utf8", windowsHide: true });
    assert.equal(result.error, undefined);
    if (temp === alias) {
      assert.equal(result.status, 0, result.stderr);
      assert.equal(result.stdout.trim(), join(alias, "termal", "tests"));
    } else {
      assert.notEqual(result.status, 0);
      assert.match(result.stderr, /parent traversal|must be absolute/);
    }
  }
});

test("failed green cleanup reports exact survivors and OS error without retry", async (t) => {
  const userTemp = sandbox(t);
  const originalRemove = fs.rmSync;
  let removals = 0;
  const mock = t.mock.method(fs, "rmSync", () => {
    removals += 1;
    throw Object.assign(new Error("injected removal failure"), { code: "EBUSY", errno: -16 });
  });
  syncBuiltinESMExports();
  try {
    await assert.rejects(child(userTemp, `
      require('node:fs').writeFileSync(require('node:path').join(process.env.TEMP, 'held.sqlite'), 'evidence');
    `), (error) => {
      assert.match(error.message, /EBUSY/);
      assert.match(error.message, /-16/);
      assert.match(error.message, /held\.sqlite/);
      assert(error.message.includes(userTemp));
      return true;
    });
    assert.equal(removals, 1);
    const [run] = readdirSync(join(userTemp, "termal", "tests"));
    assert(existsSync(join(userTemp, "termal", "tests", run, "held.sqlite")));
  } finally {
    mock.mock.restore();
    fs.rmSync = originalRemove;
    syncBuiltinESMExports();
  }
});

test("another product's new entries are informational, not TermAl failures", async (t) => {
  const userTemp = sandbox(t);
  const messages = [];
  const result = await runInTestTemp(process.execPath, ["-e", `
    require('node:fs').mkdirSync(require('node:path').join(process.env.TERMAL_TEST_USER_TEMP, 'codenav-concurrent'));
  `], { userTemp, report: (message) => messages.push(message) });
  assert.equal(result.exitCode, 0);
  assert.equal(result.afterCount, 0);
  assert(messages.some((message) => message.includes("codenav- before=0, after=1, delta=1")));
  assert.equal(existsSync(join(userTemp, "codenav-concurrent")), true);
});

test("sweep removes only stale marked runs, leaving recent and unowned directories", (t) => {
  const userTemp = sandbox(t);
  const root = join(userTemp, "termal", "tests");
  mkdirSync(root, { recursive: true });
  for (const name of ["run-old", "run-recent", "unowned"]) {
    mkdirSync(join(root, name));
  }
  for (const name of ["run-old", "run-recent"]) {
    writeFileSync(join(root, name, ".termal-test-run"), JSON.stringify({ version: 1, pid: 2147483647 }));
  }
  const old = new Date(Date.now() - 3 * 24 * 60 * 60 * 1000);
  utimesSync(join(root, "run-old", ".termal-test-run"), old, old);
  assert.deepEqual(sweepStaleTestRuns(root), [join(root, "run-old")]);
  assert.deepEqual(readdirSync(root).sort(), ["run-recent", "unowned"]);
});

test("sweep never traverses a linked run directory", (t) => {
  const userTemp = sandbox(t);
  const root = join(userTemp, "termal", "tests");
  const outside = join(userTemp, "external-data");
  mkdirSync(root, { recursive: true });
  mkdirSync(outside);
  writeFileSync(join(outside, ".termal-test-run"), JSON.stringify({ version: 1, pid: 2147483647 }));
  const old = new Date(0);
  utimesSync(join(outside, ".termal-test-run"), old, old);
  symlinkSync(outside, join(root, "run-linked"), process.platform === "win32" ? "junction" : "dir");
  assert.deepEqual(sweepStaleTestRuns(root), []);
  assert.equal(existsSync(join(outside, ".termal-test-run")), true);
});

test("sweep preserves old runs whose owner is still alive", (t) => {
  const userTemp = sandbox(t);
  const root = join(userTemp, "termal", "tests");
  const active = join(root, "run-active");
  mkdirSync(active, { recursive: true });
  const marker = join(active, ".termal-test-run");
  writeFileSync(marker, JSON.stringify({ version: 1, pid: process.pid }));
  utimesSync(marker, new Date(0), new Date(0));
  assert.deepEqual(sweepStaleTestRuns(root), []);
  assert.equal(existsSync(active), true);
});

test("sweep is bounded to 64 dead runs per invocation", (t) => {
  const userTemp = sandbox(t);
  const root = join(userTemp, "termal", "tests");
  mkdirSync(root, { recursive: true });
  for (let index = 0; index < 65; index++) {
    const path = join(root, `run-${index}`);
    mkdirSync(path);
    const marker = join(path, ".termal-test-run");
    writeFileSync(marker, JSON.stringify({ version: 1, pid: 2147483647 }));
    utimesSync(marker, new Date(0), new Date(0));
  }
  assert.equal(sweepStaleTestRuns(root).length, 64);
  assert.equal(readdirSync(root).length, 1);
});

test("cleanup path replacement is reported rather than deleted or retried", async (t) => {
  const userTemp = sandbox(t);
  await assert.rejects(child(userTemp, `
    const fs = require('node:fs');
    fs.rmSync(process.env.TERMAL_TEST_RUN_ROOT, { recursive: true });
    fs.writeFileSync(process.env.TERMAL_TEST_RUN_ROOT, 'replacement evidence');
  `), /not a plain directory/);
  assert.equal(readdirSync(join(userTemp, "termal", "tests")).length, 1);
});

test("a linked product root is rejected before spawning a child", async (t) => {
  const userTemp = sandbox(t);
  const outside = join(userTemp, "external-data");
  mkdirSync(outside);
  symlinkSync(outside, join(userTemp, "termal"), process.platform === "win32" ? "junction" : "dir");
  await assert.rejects(child(userTemp, ""), /symbolic|link/i);
  assert.deepEqual(readdirSync(outside), []);
});

test("spawn errors retain the run directory and report the failing executable", async (t) => {
  const userTemp = sandbox(t);
  await assert.rejects(runInTestTemp(join(userTemp, "absent-executable"), [], {
    userTemp,
    report: () => {},
  }), /absent-executable/);
  assert.equal(readdirSync(join(userTemp, "termal", "tests")).length, 1);
});
