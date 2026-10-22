// Real process witnesses for launcher ownership; no product tests are run here.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, symlinkSync, writeFileSync, unlinkSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import test from "node:test";
import { EventEmitter } from "node:events";
import { PassThrough } from "node:stream";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createRun, executeRun, notifyStall, recoverRun, summarize, saveProgressObservation } from "./test-launcher.mjs";
import { runOwnedCommand } from "./test-process-owner.mjs";
import { startPosixOwner } from "./test-process-owner-posix.mjs";
import { watchCommandProgress } from "./test-command-progress.mjs";

const gone = (pid) => {
  try { process.kill(pid, 0); return false; }
  catch (error) { return error.code === "ESRCH"; }
};
const tick = () => new Promise((resolve) => setTimeout(resolve, 10));
async function waitForFile(path, signal) {
  while (!existsSync(path)) {
    signal.throwIfAborted();
    await tick();
  }
  return readFileSync(path, "utf8");
}

// The only safety owner is the outer test process, never the launcher or the
// command. Otherwise the fixture could clean up the very leak it must expose.
// Readiness and release files are barriers; elapsed time decides no assertion.
function processTree(t, root, env) {
  const path = (name) => join(root, ".git", `lifecycle-${name}`);
  const leaf = path("leaf.cjs");
  writeFileSync(leaf, [
    "const fs = require('node:fs');",
    "const [ready, release, owner] = process.argv.slice(2);",
    "const ownerAlive = () => { try { process.kill(Number(owner), 0); return true; } catch (e) { return e.code !== 'ESRCH'; } };",
    "fs.writeFileSync(ready + '.partial', JSON.stringify({ pid: process.pid, parent: process.ppid }));",
    "fs.renameSync(ready + '.partial', ready);",
    "setInterval(() => { if (fs.existsSync(release) || !ownerAlive()) process.exit(0); }, 10);",
  ].join("\n"));
  const command = path("command.cjs");
  writeFileSync(command, [
    "const fs = require('node:fs');",
    "const { spawn } = require('node:child_process');",
    `const owner = ${process.pid};`,
    "const ownerAlive = () => { try { process.kill(owner, 0); return true; } catch (e) { return e.code !== 'ESRCH'; } };",
    // Match windows-containment.cjs's independent-leaf witness. libuv's
    // private Windows job kills ordinary Node children when this Node parent
    // exits. Skipping that extra job models a native Cargo/test descendant;
    // it does not request breakaway from a strict enclosing launcher job.
    `spawn(process.execPath, [${JSON.stringify(leaf)}, ${JSON.stringify(path("grandchild.pid"))}, ${JSON.stringify(path("release-grandchild"))}, String(owner)], { stdio: 'ignore', windowsHide: true, detached: process.platform === 'win32' });`,
    "fs.writeSync(1, 'running 1 test\\n');",
    `fs.writeFileSync(${JSON.stringify(path("command.pid") + ".partial")}, JSON.stringify({ pid: process.pid, parent: process.ppid }));`,
    `fs.renameSync(${JSON.stringify(path("command.pid") + ".partial")}, ${JSON.stringify(path("command.pid"))});`,
    "let progressed = false;",
    "setInterval(() => {",
    `  if (fs.existsSync(${JSON.stringify(path("release-command"))}) || !ownerAlive()) process.exit(0);`,
    `  if (!progressed && fs.existsSync(${JSON.stringify(path("request-progress"))})) {`,
    "    progressed = true; fs.writeSync(1, 'test fixture is making progress\\n');",
    `    fs.writeFileSync(${JSON.stringify(path("progress-written"))}, 'written');`,
    "  }",
    "}, 10);",
  ].join("\n"));
  const sibling = spawn(process.execPath, [
    leaf, path("sibling.pid"), path("release-sibling"), String(process.pid),
  ], { cwd: dirname(root), env, windowsHide: true, stdio: "ignore" });
  const siblingClosed = new Promise((resolve) => sibling.once("close", resolve));
  const release = (name) => writeFileSync(path(`release-${name}`), "release");
  const releaseAll = () => {
    for (const name of ["command", "grandchild", "sibling"]) release(name);
  };
  // A runner timeout may cancel the test while it is waiting for a barrier.
  // Release only this fixture's processes; never enumerate or kill by name.
  t.signal.addEventListener("abort", releaseAll, { once: true });
  let cleanupPromise;
  const cleanup = () => cleanupPromise ??= (async () => {
    releaseAll();
    await siblingClosed;
    for (const name of ["command", "grandchild"]) {
      if (!existsSync(path(`${name}.pid`))) continue;
      const { pid } = JSON.parse(readFileSync(path(`${name}.pid`), "utf8"));
      while (!gone(pid)) await tick();
    }
    t.signal.removeEventListener("abort", releaseAll);
  })();
  t.after(cleanup);
  return {
    command,
    release,
    cleanup,
    path,
    async ready(completion) {
      const read = async () => {
        const pids = {};
        const identities = {};
        for (const name of ["command", "grandchild", "sibling"]) {
          identities[name] = JSON.parse(await waitForFile(path(`${name}.pid`), t.signal));
          pids[name] = identities[name].pid;
          assert.equal(gone(pids[name]), false, `${name} must actually be alive`);
        }
        assert.equal(identities.grandchild.parent, pids.command,
          "the witness must be a real descendant, not another direct child of the test");
        assert.equal(identities.sibling.parent, process.pid);
        return pids;
      };
      return completion ? Promise.race([read(), completion.then((result) => {
        throw new Error(`fixture command ended before readiness: ${JSON.stringify(result)}`);
      })]) : read();
    },
  };
}

export function registerLauncherLifecycleTests({ repository, stage, json }) {
  // Keep agent-created fixture files inside this checkout under Greg's rule,
  // even when an outer runner selected a user/system temporary managed root.
  // The repository helper removes passing fixtures and preserves failed ones
  // here for the holder's explicit PID audit and failure investigation.
  const fixtureDirectory = resolve(dirname(fileURLToPath(import.meta.url)), "..", ".tmp", "launcher-lifecycle");
  const lifecycleRepository = (t, callback) => {
    mkdirSync(fixtureDirectory, { recursive: true });
    return repository(t, callback, undefined, fixtureDirectory);
  };
  if (process.platform === "win32") {
    for (const redirected of [".tmp", "ow"]) {
      test(`focused lifecycle: redirected ${redirected} compiler scratch refuses without touching target`, async (t) => {
        await lifecycleRepository(t, async (root, env) => {
          const copyRoot = join(root, "helper-copy");
          const scripts = join(copyRoot, "scripts");
          mkdirSync(scripts, { recursive: true });
          for (const name of ["test-process-owner.mjs", "test-process-owner-windows.mjs"]) {
            copyFileSync(fileURLToPath(new URL(name, import.meta.url)), join(scripts, name));
          }
          // Target is outside the copied helper checkout but still inside this
          // disposable repository. Neither compiler nor cleanup may touch it.
          const target = join(root, "untouched-compiler-target");
          mkdirSync(target);
          const marker = join(target, "keep.txt");
          writeFileSync(marker, "untouched");
          const temporary = join(copyRoot, ".tmp");
          if (redirected === "ow") mkdirSync(temporary);
          const link = redirected === ".tmp" ? temporary : join(temporary, "ow");
          symlinkSync(target, link, "junction");
          try {
            const { runOwnedCommand: runCopied } = await import(pathToFileURL(join(scripts, "test-process-owner.mjs")));
            const commandMarker = join(root, ".git", "redirected-command-must-not-run");
            const outcome = await runCopied(process.execPath, ["-e", `require('node:fs').writeFileSync(${JSON.stringify(commandMarker)}, 'ran')`], {
              cwd: root, env, log: join(root, ".git", "redirected-compiler.log"),
            });
            assert.equal(outcome.code, 1);
            assert.match(outcome.error, /compiler scratch refuses redirected directory/u);
            assert.equal(outcome.bootstrapPid, undefined, "PowerShell must not start");
            assert.equal(existsSync(commandMarker), false);
            assert.deepEqual(readdirSync(target), ["keep.txt"], "no scratch or child directories may be created through the junction");
            assert.equal(readFileSync(marker, "utf8"), "untouched");
          } finally {
            // Remove the fixture junction itself before repository cleanup.
            unlinkSync(link);
          }
        });
      });
    }
  }
  test("focused lifecycle: owner exit and stderr survive a missing result ledger", async (t) => {
    await lifecycleRepository(t, async (root, env) => {
      const outcome = await runOwnedCommand(process.execPath, [], {
        cwd: root, env, log: join(root, ".git", "bootstrap.log"),
      }, { spawnOwner: () => {
        const owner = new EventEmitter();
        owner.stdin = new PassThrough(); owner.stdout = new PassThrough(); owner.stderr = new PassThrough();
        queueMicrotask(() => {
          // ChildProcess close is after stdio delivery. Model that ordering
          // explicitly instead of relying on PassThrough nextTick scheduling.
          owner.stderr.emit("data", "fixture bootstrap refused\n");
          owner.emit("close", 71, null);
        });
        return owner;
      } });
      assert.equal(outcome.code, 1);
      assert.equal(outcome.cleanup.state, "unresolved");
      assert.deepEqual(outcome.ownerDiagnostics, { code: 71, signal: null, stderr: "fixture bootstrap refused\n" });
      assert.match(outcome.error, /exit=71.*fixture bootstrap refused/u);
      assert.doesNotMatch(outcome.error, /ENOENT/u);
      const runDir = await createRun({ root, stages: [stage("focused")] }, env);
      const entry = { name: "focused", state: "failed", ...outcome,
        ownerDiagnostics: { ...outcome.ownerDiagnostics, stderr: outcome.ownerDiagnostics.stderr + "x".repeat(5000) } };
      writeFileSync(join(runDir, "results.json"), JSON.stringify({ ...json(join(runDir, "results.json")),
        state: "failed", exitCode: 1, ended: new Date().toISOString(), stages: [entry] }));
      const summary = await summarize(runDir);
      assert.match(summary, /owner exit=71 signal=none stderr=fixture bootstrap refused/u);
      assert.ok(summary.split("\n").find((line) => line.startsWith("owner exit=")).length <= 2400);
      assert.doesNotMatch(summary, /x{5000}/u);
    });
  });

  test("focused lifecycle: an early POSIX interrupt is repeated after the owner attaches its listener", async (t) => {
    await lifecycleRepository(t, async (root, env) => {
      const controller = new AbortController();
      let owner, request, sent = 0, started;
      const acknowledged = new Promise((done) => { started = done; });
      const running = runOwnedCommand("fixture", [], {
        cwd: root, env, log: join(root, ".git", "early-interrupt.log"), signal: controller.signal,
        onOwner: () => controller.abort("before ESM listener"),
      }, { platform: "linux", spawnOwner: (_program, args) => {
        request = json(args.at(-1));
        owner = new EventEmitter(); owner.pid = 2; owner.connected = true;
        owner.stderr = new PassThrough();
        // The first send is deliberately lost before the simulated listener.
        owner.send = (_message, callback) => { sent += 1; callback(); };
        queueMicrotask(() => { owner.emit("message", { type: "started", nonce: request.nonce, pid: 3 }); started(); });
        return owner;
      } });
      await acknowledged;
      try { assert.equal(sent, 2, "the acknowledgement must replay the previously lost interrupt"); }
      finally {
        const result = { nonce: request.nonce, code: sent === 2 ? 1 : 0, interrupted: sent === 2,
          cleanup: { state: sent === 2 ? "reaped" : "unresolved", trigger: "fixture", pids: [] } };
        writeFileSync(request.ledger, JSON.stringify(result));
        owner.emit("message", { type: "finished", nonce: request.nonce, result });
        owner.emit("close", 0, null);
        const outcome = await running;
        assert.equal(outcome.interrupted, true);
        assert.equal(outcome.cleanup.state, "reaped");
      }
    });
  });

  for (const exhausted of [false, true]) {
    test(`focused lifecycle: POSIX startup IPC loss ${exhausted ? "settles unresolved at the cleanup budget" : "reaps before readiness"}`, async () => {
      // Real production state machine; only the OS and transport are injected.
      // No Unix signal or pretend OS PID is sent on this Windows test host.
      const parent = new EventEmitter(); parent.connected = true; parent.env = {}; parent.pid = 1;
      parent.disconnect = () => { parent.connected = false; parent.emit("disconnect"); };
      parent.send = (message, callback) => callback(message.type === "started" ? new Error("EPIPE") : undefined);
      const child = new EventEmitter(); child.pid = 2;
      let unreferenced = false, alive = true, clock = 0;
      child.unref = () => { unreferenced = true; };
      const killed = [], order = [];
      const owner = startPosixOwner({ command: "fixture", args: [], cleanupBudgetMs: 5000, nonce: "fixture" }, {
        parent, spawnCommand: () => child, openLog: () => 7,
        closeLog: () => order.push("closed"),
        killGroup: (pid) => { killed.push(pid); if (!exhausted) alive = false; },
        groupAlive: () => alive, now: () => clock,
        sleep: async () => { clock += 1000; },
        persist: (result) => order.push(result),
      });
      child.emit("spawn");
      // The startup send callback must trigger cleanup before any readiness.
      await Promise.resolve(); await Promise.resolve();
      const result = await owner.finish("crossed-shutdown");
      assert.deepEqual(killed, [2]);
      assert.equal(result.code, 1);
      assert.equal(result.interrupted, true);
      assert.equal(result.cleanup.trigger, "launcher-exit");
      assert.equal(result.cleanup.state, exhausted ? "unresolved" : "reaped");
      assert.equal(unreferenced, true, "a root that never emits exit must not hold the guardian open");
      assert.equal(parent.connected, false);
      assert.equal(order[0], result, "durable failure precedes releasing the child reference");
      assert.equal(order[1], "closed");
      assert.equal(alive, exhausted);
      if (exhausted) assert.equal(clock, 5000);
    });
  }

  for (const outcome of ["settled", "permission-denied", "live", "unexpected-signal-error"]) {
    test(`focused lifecycle: POSIX permission result requires an absent-group probe (${outcome})`, async () => {
      const parent = new EventEmitter(); parent.connected = true; parent.env = {}; parent.pid = 1;
      parent.disconnect = () => { parent.connected = false; parent.emit("disconnect"); };
      parent.send = (_message, callback) => callback();
      const child = new EventEmitter(); child.pid = 2; child.unref = () => {};
      let signals = 0, probes = 0, clock = 0;
      const permission = () => Object.assign(new Error("kill EPERM"), { code: "EPERM" });
      const owner = startPosixOwner({ command: "fixture", args: [], cleanupBudgetMs: 5000, nonce: "fixture" }, {
        parent, spawnCommand: () => child, openLog: () => 7, closeLog: () => {}, persist: () => {},
        killGroup: () => {
          signals += 1;
          if (outcome === "unexpected-signal-error") throw Object.assign(new Error("signal EIO"), { code: "EIO" });
          throw permission();
        },
        groupAlive: () => {
          probes += 1;
          if (outcome === "settled" && probes > 1) return false; // ESRCH, only after reaping.
          if (outcome === "live") return true;
          throw permission();
        },
        now: () => clock, sleep: async () => { clock += 1000; },
      });
      child.emit("exit", 0, null);
      const result = await owner.finish("crossed-finish");
      assert.equal(signals, 1, "never signal a potentially reused group again");
      assert.equal(result.cleanup.state, outcome === "settled" ? "reaped" : "unresolved");
      assert.equal(result.code, outcome === "settled" ? 0 : 1);
      if (outcome === "settled") {
        assert.equal(probes, 2);
        assert.equal(clock, 1000);
        assert.equal(result.error, undefined);
      } else if (outcome === "unexpected-signal-error") {
        assert.equal(probes, 0);
        assert.match(result.error, /EIO/u);
      } else {
        assert.equal(clock, 5000, "keep the existing cleanup bound");
        assert.match(result.error, /signal=EPERM/u);
        assert.match(result.error, outcome === "live" ? /probe=present/u : /probe=EPERM/u);
      }
    });
  }

  test("focused lifecycle: failed stall and resume saves recover without more output and allow episode two", async (t) => {
    await lifecycleRepository(t, async (root) => {
      const log = join(root, ".git", "observer.log"); writeFileSync(log, "");
      let clock = 0, fail = true;
      const observations = [], errors = [];
      const entry = {}, result = { stages: [entry] };
      t.mock.timers.enable({ apis: ["setInterval"] });
      const stop = watchCommandProgress({ log, pid: 1, runId: "fixture", stage: "focused",
        observe: (event) => saveProgressObservation("fixture", result, entry, event, () => {
          if (fail) throw new Error("fixture save refused"); observations.push(entry.stall);
        }),
      }, { now: () => clock, stallAfterMs: 5000, onObserverError: (error) => errors.push(error.message) });
      try {
        clock = 5000;
        assert.doesNotThrow(() => t.mock.timers.tick(1000));
        assert.deepEqual(errors, ["fixture save refused"]);
        assert.equal(entry.stall, undefined, "a later save must not persist an unreported stall");
        fail = false; t.mock.timers.tick(1000);
        assert.equal(observations[0].episode, 1);
        assert.equal(observations[0].state, "stalled");
        writeFileSync(log, "new output"); clock = 6000; fail = true;
        assert.doesNotThrow(() => t.mock.timers.tick(1000));
        assert.equal(errors.length, 2);
        assert.equal(entry.stall.state, "stalled", "failed resume must restore the last committed observation");
        // No new bytes after the failed resume. A later poll must deliver it.
        fail = false; clock = 7000; t.mock.timers.tick(1000);
        assert.equal(observations[1].state, "resumed");
        clock = 11000; t.mock.timers.tick(4000);
        assert.deepEqual(observations.map(({ state, episode }) => ({ state, episode })), [
          { state: "stalled", episode: 1 }, { state: "resumed", episode: 1 }, { state: "stalled", episode: 2 },
        ]);
        unlinkSync(log); t.mock.timers.tick(3000);
        assert.equal(observations.filter(({ state }) => state === "unknown").length, 1);
        writeFileSync(log, "readable again"); t.mock.timers.tick(1000);
        unlinkSync(log); t.mock.timers.tick(3000);
        assert.equal(observations.filter(({ state }) => state === "unknown").length, 2);
      } finally { stop(); t.mock.timers.reset(); }
    });
  });
  test("focused lifecycle: interruption reaps a real grandchild even when the command exits concurrently", async (t) => {
    await lifecycleRepository(t, async (root, sourceEnv) => {
      const env = { ...sourceEnv };
      delete env.NODE_TEST_CONTEXT;
      const tree = processTree(t, root, env);
      const controller = new AbortController();
      const runDir = await createRun({ root, stages: [{
        name: "focused", command: process.execPath, args: [tree.command],
      }] }, env);
      const running = executeRun(runDir, env, { signal: controller.signal });
      try {
        const pids = await tree.ready(running);
        t.diagnostic(`owned command=${pids.command}, grandchild=${pids.grandchild}; unrelated sibling=${pids.sibling}`);
        controller.abort("fixture interrupt");
        controller.abort("crossed second interrupt");
        // Expose the race where the command closes during teardown. The
        // grandchild has a separate barrier and cannot exit through this one.
        tree.release("command");
        const result = await running;
        assert.equal(gone(pids.grandchild), true,
          `launcher returned with its real grandchild ${pids.grandchild} still alive`);
        assert.equal(gone(pids.sibling), false, "an unrelated process must survive");
        assert.equal(result.state, "failed", "interrupt is never a passing run");
        assert.equal(result.stages[0].interrupted, true);
        assert.equal(result.stages[0].cleanup.state, "reaped");
        assert.equal(result.stages[0].pid, pids.command);
        assert.match(result.stages[0].cleanup.trigger, /interrupt/u);
        assert.equal(json(join(runDir, "results.json")).state, "failed");
        assert.match(await summarize(runDir), /cleanup=reaped/u);
      } finally {
        tree.release("command");
        tree.release("grandchild");
        await running;
        await tree.cleanup();
      }
    });
  });

  test("focused lifecycle: a quiet live test reports a stall, progress resets it, and it never terminates the run", async (t) => {
    await lifecycleRepository(t, async (root, sourceEnv) => {
      const env = { ...sourceEnv };
      delete env.NODE_TEST_CONTEXT;
      const tree = processTree(t, root, env);
      const runDir = await createRun({ root, stages: [{
        name: "focused", command: process.execPath, args: [tree.command],
      }] }, env);
      let clock = 0;
      const notices = [];
      let markWatching;
      const watching = new Promise((done) => { markWatching = done; });
      t.mock.timers.enable({ apis: ["setInterval"] });
      const running = executeRun(runDir, env, {
        progress: {
          now: () => clock, stallAfterMs: 5000, pollMs: 1000,
          onWatching: markWatching,
          notify: (observation) => { notices.push(observation); },
        },
      });
      try {
        const pids = await tree.ready(running);
        await watching; // The command's pid file alone does not prove installation.
        // Consume the initial readiness output before advancing inactivity.
        t.mock.timers.tick(1000);
        clock = 5000;
        t.mock.timers.tick(5000);
        const stalled = json(join(runDir, "results.json"));
        assert.equal(stalled.stages[0].stall?.state, "stalled",
          "a quiet active test must leave a durable stall observation");
        assert.equal(stalled.state, "running");
        assert.equal(gone(pids.command), false);
        assert.equal(gone(pids.grandchild), false);
        assert.equal(notices.length, 1, "one notice, not one per polling tick");
        assert.equal(notices[0].pid, pids.command);
        assert.equal(notices[0].runId, stalled.runId);
        assert.equal(notices[0].stage, "focused");
        assert.equal(notices[0].inactiveMs, 5000);
        assert.equal(notices[0].log, join(runDir, "focused.log"));
        assert.match(await summarize(runDir), /progress=stalled episode=1 inactive-ms=5000/u);
        writeFileSync(tree.path("request-progress"), "progress");
        await waitForFile(tree.path("progress-written"), t.signal);
        clock = 6000;
        t.mock.timers.tick(1000);
        assert.equal(json(join(runDir, "results.json")).stages[0].stall.state, "resumed");
        assert.match(await summarize(runDir), /progress=resumed episode=1 inactive-ms=0/u);
        clock = 10000;
        t.mock.timers.tick(4000);
        assert.equal(notices.length, 1, "recent output must suppress a stall");
        clock = 11000;
        t.mock.timers.tick(1000);
        assert.equal(notices.length, 2, "a later quiet episode is visible");
        assert.equal(json(join(runDir, "results.json")).state, "running");
      } finally {
        t.mock.timers.reset();
        tree.release("command");
        tree.release("grandchild");
        await running;
        await tree.cleanup();
      }
    });
  });

  test("focused lifecycle: normal success preserves native counts and executes once", async (t) => {
    await lifecycleRepository(t, async (root, env) => {
      const counter = join(root, ".git", "lifecycle-count");
      const runDir = await createRun({ root, stages: [stage("focused", [
        `require('node:fs').appendFileSync(${JSON.stringify(counter)}, 'executed\\n');`,
        "console.log('test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out');",
      ].join("\n"))] }, env);
      const result = await executeRun(runDir, env);
      assert.equal(result.state, "passed");
      assert.equal(result.stages[0].tests.passed, 1);
      assert.equal(readFileSync(counter, "utf8"), "executed\n");
    });
  });

  test("focused lifecycle: successful root exit reaps a still-live descendant and preserves its unrelated sibling", async (t) => {
    await lifecycleRepository(t, async (root, sourceEnv) => {
      const env = { ...sourceEnv }; delete env.NODE_TEST_CONTEXT;
      const tree = processTree(t, root, env);
      const runDir = await createRun({ root, stages: [{ name: "focused", command: process.execPath, args: [tree.command] }] }, env);
      const running = executeRun(runDir, env);
      try {
        const pids = await tree.ready(running);
        tree.release("command"); // The grandchild release barrier stays closed.
        const result = await running;
        assert.equal(result.state, "passed", JSON.stringify(result));
        assert.equal(result.stages[0].cleanup.trigger, "command-exit");
        assert.equal(result.stages[0].cleanup.state, "reaped");
        assert.equal(gone(pids.grandchild), true);
        assert.equal(gone(pids.sibling), false);
        t.diagnostic(`normal-exit root=${pids.command}, leaf=${pids.grandchild} gone; sibling=${pids.sibling} survives`);
      } finally { tree.release("command"); tree.release("grandchild"); await running; await tree.cleanup(); }
    });
  });

  test("focused lifecycle: the independent grandchild control actually survives its parent without launcher ownership", async (t) => {
    await lifecycleRepository(t, async (root, sourceEnv) => {
      const env = { ...sourceEnv };
      delete env.NODE_TEST_CONTEXT;
      const tree = processTree(t, root, env);
      const command = spawn(process.execPath, [tree.command], {
        cwd: dirname(root), env, windowsHide: true, stdio: "ignore",
      });
      const commandClosed = new Promise((resolveClose) => command.once("close", resolveClose));
      try {
        const pids = await tree.ready();
        assert.equal(pids.command, command.pid);
        tree.release("command");
        await commandClosed;
        assert.equal(gone(pids.grandchild), false,
          "the fixture must expose a surviving descendant when no launcher owns its tree");
        assert.equal(gone(pids.sibling), false);
        t.diagnostic(`control parent=${pids.command} exited; independent grandchild=${pids.grandchild} still alive; sibling=${pids.sibling}`);
      } finally {
        tree.release("command");
        tree.release("grandchild");
        await commandClosed;
        await tree.cleanup();
      }
    });
  });

  for (const termination of process.platform === "win32" ? ["SIGTERM", "SIGKILL"] : ["SIGINT", "SIGTERM", "SIGKILL"]) {
    test(`focused lifecycle: ${termination} of the real launcher still reaps descendants and leaves a recovery ledger`, async (t) => {
      await lifecycleRepository(t, async (root, sourceEnv) => {
        const env = { ...sourceEnv };
        delete env.NODE_TEST_CONTEXT;
        const tree = processTree(t, root, env);
        const runDir = await createRun({ root, stages: [{
          name: "focused", command: process.execPath, args: [tree.command],
        }] }, env);
        const launcher = fileURLToPath(new URL("./test-launcher.mjs", import.meta.url));
        const worker = spawn(process.execPath, [launcher, "_run", runDir], {
          cwd: root, env, windowsHide: true, stdio: ["ignore", "pipe", "pipe", "ipc"],
        });
        worker.on("message", (message) => {
          if (message?.type === "READY" && worker.connected) worker.disconnect();
        });
        let output = "";
        worker.stdout.on("data", (chunk) => { output = (output + chunk).slice(-8192); });
        worker.stderr.on("data", (chunk) => { output = (output + chunk).slice(-8192); });
        // Node may omit child close after parent disconnect: https://github.com/nodejs/node/issues/65646
        const exited = new Promise((resolveExit) => worker.once("exit", (code, signal) => resolveExit({ code, signal })));
        const stdioClosed = [worker.stdout, worker.stderr].map((stream) =>
          new Promise((resolveClose) => stream.once("close", resolveClose)));
        const closed = Promise.all([exited, ...stdioClosed]).then(([{ code, signal }]) => ({ code, signal, output }));
        try {
          const pids = await tree.ready(closed);
          const owner = json(join(runDir, "results.json")).stages[0].processOwner;
          assert.ok(owner.pid > 0 && owner.pid !== pids.command);
          worker.kill(termination); // Only the exact fixture ChildProcess handle.
          await closed;
          const ledger = JSON.parse(await waitForFile(owner.ledger, t.signal));
          // The ledger is published after native job/group cleanup, not after
          // merely asking a reusable PID to stop.
          assert.equal(ledger.nonce, owner.nonce);
          assert.equal(ledger.cleanup.state, "reaped", JSON.stringify(ledger));
          assert.equal(gone(pids.command), true);
          assert.equal(gone(pids.grandchild), true);
          assert.equal(gone(pids.sibling), false);
          while (!gone(owner.pid)) await tick();
          const current = json(join(runDir, "results.json"));
          assert.notEqual(current.state, "passed");
          if (termination !== "SIGKILL" && process.platform !== "win32") {
            assert.equal(current.interrupted, true, "native signal must pass through the launcher's abort handler");
          }
          if (!current.ended) {
            await recoverRun(runDir, env);
            const recovered = json(join(runDir, "results.json"));
            assert.equal(recovered.state, "failed");
            assert.equal(recovered.interrupted, true);
            assert.equal(recovered.stages[0].cleanup.state, "reaped");
            assert.match(await summarize(runDir), /cleanup=reaped.*scope=(best-effort job snapshot before termination|command root only)/u);
          }
          t.diagnostic(`${termination}: launcher=${worker.pid}, command=${pids.command}, leaf=${pids.grandchild}, owner=${owner.pid} gone; sibling=${pids.sibling} unaffected`);
        } finally {
          tree.release("command"); tree.release("grandchild");
          if (worker.exitCode === null && worker.signalCode === null) worker.kill();
          await closed;
          await tree.cleanup();
        }
      });
    });
  }

  test("focused lifecycle: cancellation before launch never starts a command", async (t) => {
    await lifecycleRepository(t, async (root, env) => {
      const marker = join(root, ".git", "must-not-start");
      const runDir = await createRun({ root, stages: [stage("focused",
        `require('node:fs').writeFileSync(${JSON.stringify(marker)}, 'started');`)] }, env);
      const controller = new AbortController(); controller.abort("fixture timeout");
      const result = await executeRun(runDir, env, { signal: controller.signal });
      assert.equal(result.state, "failed");
      assert.equal(result.stages[0].interrupted, true);
      assert.equal(existsSync(marker), false);
    });
  });

  if (process.platform === "win32") {
    test("focused lifecycle: cancellation during Windows owner bootstrap refuses command admission", async (t) => {
      await lifecycleRepository(t, async (root, env) => {
        const marker = join(root, ".git", "bootstrap-must-not-start");
        const controller = new AbortController();
        const outcome = await runOwnedCommand(process.execPath, ["-e",
          `require('node:fs').writeFileSync(${JSON.stringify(marker)}, 'started');`], {
          cwd: root, env, log: join(root, ".git", "bootstrap-cancel.log"), signal: controller.signal,
          // Environment admission is blocked until this callback returns;
          // this is a barrier, not an assumption about compiler speed.
          onOwner: () => controller.abort("buffered bootstrap interrupt"),
        });
        assert.equal(outcome.interrupted, true);
        assert.equal(outcome.code, 1);
        assert.equal(outcome.pid, null, "native command must not have been created");
        assert.equal(outcome.cleanup.state, "reaped", JSON.stringify(outcome));
        assert.equal(existsSync(marker), false);
        t.diagnostic(`bootstrap-cancel owner=${outcome.ownerPid} exited; no command admitted`);
      });
    });
  }

  test("focused lifecycle: ownership preserves argv and caller environment", async (t) => {
    await lifecycleRepository(t, async (root, sourceEnv) => {
      const record = join(root, ".git", "owner-parity.json");
      const values = ["", "space and quote\"", "trailing\\", "zażółć", "$() & literal"];
      const env = { ...sourceEnv, TERMAL_OWNER_FIXTURE_VALUE: "literal ć & value" };
      const code = `require('node:fs').writeFileSync(${JSON.stringify(record)}, JSON.stringify({ args: process.argv.slice(1), value: process.env.TERMAL_OWNER_FIXTURE_VALUE, modules: process.env.PSModulePath ?? null }));`;
      const runDir = await createRun({ root, stages: [{ name: "focused", command: process.execPath,
        args: ["-e", code, "--", ...values] }] }, env);
      const result = await executeRun(runDir, env);
      assert.equal(result.state, "passed", JSON.stringify(result));
      assert.deepEqual(json(record), { args: values, value: env.TERMAL_OWNER_FIXTURE_VALUE, modules: env.PSModulePath ?? null });
      if (process.platform === "win32") {
        // Compare against Node's real native environment selection, including
        // both insertion orders of case-insensitive duplicate key spellings.
        const read = "process.stdout.write(JSON.stringify({value:process.env.TERMAL_OWNER_DUP, target:process.env.CARGO_TARGET_DIR}))";
        for (const duplicates of [
          { termal_owner_dup: "lower", TERMAL_OWNER_DUP: "upper", cargo_target_dir: "lower-target", CARGO_TARGET_DIR: "upper-target" },
          { TERMAL_OWNER_DUP: "upper", termal_owner_dup: "lower", CARGO_TARGET_DIR: "upper-target", cargo_target_dir: "lower-target" },
        ]) {
          const duplicateEnv = { ...env, ...duplicates };
          const native = spawnSync(process.execPath, ["-e", read], { cwd: root, env: duplicateEnv, encoding: "utf8" });
          assert.equal(native.status, 0, native.stderr);
          const run = await createRun({ root, stages: [{ name: "focused", command: process.execPath, args: ["-e", read] }] }, duplicateEnv);
          const outcome = await executeRun(run, duplicateEnv);
          assert.equal(outcome.state, "passed", JSON.stringify(outcome));
          assert.deepEqual(JSON.parse(readFileSync(join(run, "focused.log"), "utf8")), JSON.parse(native.stdout));
        }
      }
    });
  });

  test("focused lifecycle: a stall notice has its own key and does not consume completion or restart anything", async (t) => {
    await lifecycleRepository(t, async (root, env) => {
      const runDir = await createRun({ root, stages: [stage("focused")], notifyTo: "fixture-parent" }, env);
      const before = readFileSync(join(runDir, "results.json"), "utf8");
      const calls = [];
      const observation = { runId: json(join(runDir, "request.json")).runId,
        stage: "focused", pid: 12345, log: join(runDir, "focused.log"), inactiveMs: 300000, episode: 1 };
      await notifyStall(runDir, observation, env, async (command, args, options) => {
        calls.push({ command, args, options }); return { code: 0, signal: null };
      });
      assert.equal(calls.length, 1);
      assert.equal(calls[0].args[calls[0].args.indexOf("--idempotency-key") + 1], `termal-tests:${observation.runId}:stall-focused-1`);
      assert.equal(readFileSync(join(runDir, "results.json"), "utf8"), before);
      assert.equal(existsSync(join(runDir, "notification.json")), false);
      assert.match(readFileSync(join(runDir, "stall-focused-1.message.txt"), "utf8"), /pid=12345/u);
    });
  });
}
