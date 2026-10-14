// Test-only registration witnesses mounted by scripts/test-launcher.test.mjs.
// Owns the held host acknowledgement and execution barriers; it does not own
// registration, run indexing, credit, or restart policy in the product.
import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { basename, join } from "node:path";
import test from "node:test";
import { createRun, executeRun, startDetachedRun } from "./test-launcher.mjs";

function barrier() {
  let release;
  const ready = new Promise((resolve) => { release = resolve; });
  return { ready, release };
}

async function registrationHost(callback, {
  status = 201, registered = true, autoAcknowledge = false,
  acknowledgement, rawAcknowledgement, queryStatus = 200, queryTransform,
  loseFirstReply = false, loseAllReplies = false,
} = {}) {
  const received = barrier();
  const acknowledge = barrier();
  const requests = [];
  const queries = [];
  const registrations = new Map();
  if (autoAcknowledge) acknowledge.release();
  const server = createServer(async (request, response) => {
    try {
      if (request.method === "GET") {
        queries.push(request.url);
        const runId = decodeURIComponent(request.url.slice("/api/test-runs/".length));
        const saved = registrations.get(runId);
        response.writeHead(saved ? queryStatus : 404, { "content-type": "application/json" });
        const run = saved ? {
          runId, runDir: saved.runDirectory, worktree: saved.request.root,
          ownerSessionId: saved.request.owner, initiatingSessionId: saved.request.owner,
          creatorPid: saved.request.creatorPid, startedAt: saved.request.started,
          detached: saved.request.detached, registrationState: "registered", trackingDurability: "durable",
          trackingProvenances: ["launcherRegistered"],
        } : undefined;
        response.end(JSON.stringify({ run: queryTransform ? queryTransform(run) : run }));
        return;
      }
      let body = "";
      for await (const chunk of request) body += chunk;
      const record = { method: request.method, path: request.url, body: JSON.parse(body), rawBody: body };
      requests.push(record);
      if (!registrations.has(record.body.runId)) {
        registrations.set(record.body.runId, {
          runDirectory: record.body.runDirectory,
          request: JSON.parse(readFileSync(join(record.body.runDirectory, "request.json"), "utf8")),
        });
      }
      received.release(record);
      await acknowledge.ready;
      if (loseAllReplies || (loseFirstReply && requests.length === 1)) {
        response.destroy();
        return;
      }
      response.writeHead(status, { "content-type": "application/json" });
      const normalAck = {
        contract: "termal-durable-test-run-registration", version: 1, runId: record.body.runId, registered,
      };
      response.end(rawAcknowledgement ?? JSON.stringify(typeof acknowledgement === "function"
        ? acknowledgement(normalAck) : acknowledgement ?? normalAck));
    } catch (error) {
      received.release({ error });
      response.writeHead(400);
      response.end();
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  try {
    await callback({
      url: `http://127.0.0.1:${server.address().port}`,
      received: received.ready,
      acknowledge: acknowledge.release,
      requests,
      queries,
      registrations,
    });
  } finally {
    acknowledge.release();
    server.closeAllConnections();
    await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
}

// Every ordinary launcher fixture receives a local registration service too.
// The outer real launcher retains its actual host and session environment.
export async function withHostedRegistrationFixture(root, env, callback) {
  await registrationHost(async (host) => {
    await callback({ ...env, TERMAL_BASE_URL: host.url });
  }, { autoAcknowledge: true });
}

function assertNoExecution(root, runDir) {
  assert.equal(existsSync(join(root, "stage-after-registration-error")), false);
  assert.equal(existsSync(join(root, "target")), false, "registration refusal must not bootstrap Cargo");
  if (!runDir) return;
  assert.equal(existsSync(join(runDir, "input.json")), false, "registration admission precedes fingerprint preflight");
  assert.equal(readdirSync(runDir).some((name) => name.startsWith("preflight-")), false);
  const result = JSON.parse(readFileSync(join(runDir, "results.json"), "utf8"));
  assert.deepEqual(result.preflight, []);
  assert.ok(result.stages.every((entry) => entry.state === "unrun"));
}

async function refusedLaunch(root, env, detached, workerExit) {
  let runDir;
  const executed = join(root, "stage-after-registration-error");
  let error;
  try {
    runDir = await createRun({ root, detached, full: detached, scope: detached ? undefined : "focused",
      needsCargo: true,
      onRunCreated: (directory) => { runDir = directory; },
      stages: [{ name: detached ? "rust-tests" : "focused", command: process.execPath,
        args: ["-e", `require('node:fs').writeFileSync(${JSON.stringify(executed)}, 'ran')`] }],
      ...(detached ? { notifyTo: "fixture-coordinator" } : {}),
    }, env);
    if (detached) {
      const worker = await startDetachedRun(runDir, env);
      await workerExit(worker);
    } else {
      await executeRun(runDir, env);
    }
  } catch (failure) { error = failure; }
  assert.ok(error, "the launch must be refused before any executor is admitted");
  assert.match(error.message, /registration/iu);
  assertNoExecution(root, runDir);
}

export function registerLauncherRegistrationTests({ repository, isolatedDetachedRepository, stage, workerExit }) {
  for (const detached of [false, true]) {
    test(`host registration is acknowledged before the first ${detached ? "detached full" : "foreground focused"} stage`, async (t) => {
      const fixture = detached ? isolatedDetachedRepository : repository;
      await fixture(t, async (root, originalEnv) => {
        await registrationHost(async (host) => {
          const env = { ...originalEnv, TERMAL_BASE_URL: host.url, TERMAL_SESSION_ID: "fixture-owner" };
          const acknowledged = join(root, ".git", "registration-acknowledged");
          const executed = join(root, ".git", "registered-stage-count");
          const execution = (async () => {
            // A full-mode fixture uses one harmless stage, not the maintained
            // five-stage gate. Its neutral name does not identify Node as the
            // Rust test shell; the full mode and worker handoff are real.
            const runDir = await createRun({
              root,
              full: detached,
              needsCargo: true,
              scope: detached ? undefined : "focused",
              detached,
              stages: [stage(detached ? "registration-witness" : "focused", [
                "const fs = require('node:fs');",
                `if (!fs.existsSync(${JSON.stringify(acknowledged)})) process.exit(17);`,
                `fs.appendFileSync(${JSON.stringify(executed)}, 'once\\n');`,
                "console.log('test result: ok. 1 passed; 0 failed; 0 ignored;');",
              ].join("\n"))],
              ...(detached ? { notifyTo: "fixture-coordinator" } : {}),
            }, env);
            if (detached) {
              const worker = await startDetachedRun(runDir, env);
              assert.deepEqual(await workerExit(worker), { code: 0, signal: null });
            } else {
              assert.equal((await executeRun(runDir, env)).state, "passed");
            }
            const result = JSON.parse(readFileSync(join(runDir, "results.json"), "utf8"));
            assert.equal(result.state, "passed");
            assert.deepEqual(result.preflight.map(({ name, code }) => ({ name, code })), [{ name: "cargo", code: 0 }]);
            return runDir;
          })();
          // Completion races receipt rather than a timer: on the unchanged
          // launcher the stage ends with no POST, producing a finite RED.
          const finished = execution.then(
            (runDir) => ({ kind: "finished", runDir }),
            (error) => ({ kind: "finished", error }),
          );
          try {
            const first = await Promise.race([
              host.received.then((request) => ({ kind: "registered", request })),
              finished,
            ]);
            assert.equal(first.kind, "registered", "launcher executed without registering with the host");
            assert.equal(first.request.error, undefined);
            assert.equal(first.request.method, "POST");
            assert.equal(first.request.path, "/api/sessions/fixture-owner/test-runs");
            assert.equal(existsSync(executed), false, "a stage cannot start while acknowledgement is held");
            const runId = first.request.body.runId;
            const runDir = join(root, ".git", "review-runs", runId);
            assertNoExecution(root, runDir);
            assert.equal(basename(runDir), runId);
            const request = JSON.parse(readFileSync(join(runDir, "request.json"), "utf8"));
            assert.equal(request.runId, runId);
            assert.equal(request.owner, "fixture-owner");
            assert.equal(request.detached, detached);
            writeFileSync(acknowledged, "acknowledged\n");
            host.acknowledge();
            const outcome = await finished;
            assert.equal(outcome.error, undefined, outcome.error?.stack);
            assert.equal(outcome.runDir, runDir);
            assert.equal(readFileSync(executed, "utf8"), "once\n");
            assert.equal(host.requests.length, 1, "worker handoff must not register a second run");
          } finally {
            host.acknowledge();
            await finished;
          }
        });
      });
    });

    for (const refused of [
      { label: "refused acknowledgement", status: 409, registered: false },
      { label: "persistence failure", status: 503, registered: false },
      { label: "unconfirmed acknowledgement", status: 201, registered: false },
    ]) {
      test(`host registration failure aborts ${detached ? "detached" : "foreground"} execution: ${refused.label}`, async (t) => {
        const fixture = detached ? isolatedDetachedRepository : repository;
        await fixture(t, async (root, originalEnv) => {
          await registrationHost(async (host) => {
            host.acknowledge();
            const env = { ...originalEnv, TERMAL_BASE_URL: host.url, TERMAL_SESSION_ID: "fixture-owner" };
            const executed = join(root, ".git", "stage-after-refusal");
            let result;
            let launchError;
            try {
              const runDir = await createRun({
                root,
                detached,
                full: detached,
                scope: detached ? undefined : "focused",
                stages: [stage("focused", `require('node:fs').writeFileSync(${JSON.stringify(executed)}, 'ran')`)],
                ...(detached ? { notifyTo: "fixture-coordinator" } : {}),
              }, env);
              if (detached) {
                const worker = await startDetachedRun(runDir, env);
                await workerExit(worker);
                result = JSON.parse(readFileSync(join(runDir, "results.json"), "utf8"));
              } else {
                result = await executeRun(runDir, env);
              }
            } catch (error) {
              launchError = error;
            }
            assert.equal(existsSync(executed), false, "registration failure must not start a stage");
            assert.ok(launchError || (Number.isInteger(result?.exitCode) && result.exitCode !== 0), "registration failure must abort the launch");
            assert.match(String(launchError?.message ?? result?.error), /registration/iu);
            assert.equal(host.requests.length, 1);
          }, refused);
        });
      });
    }

    test(`host registration failure aborts ${detached ? "detached" : "foreground"} execution: malformed partial host environment`, async (t) => {
      const fixture = detached ? isolatedDetachedRepository : repository;
      await fixture(t, async (root, originalEnv) => {
        const env = { ...originalEnv, TERMAL_BASE_URL: "not-a-host-url", TERMAL_SESSION_ID: "fixture-owner" };
        const executed = join(root, ".git", "stage-after-malformed-host");
        let result;
        let launchError;
        try {
          const runDir = await createRun({
            root,
            detached,
            full: detached,
            scope: detached ? undefined : "focused",
            stages: [stage("focused", `require('node:fs').writeFileSync(${JSON.stringify(executed)}, 'ran')`)],
            ...(detached ? { notifyTo: "fixture-coordinator" } : {}),
          }, env);
          if (detached) {
            const worker = await startDetachedRun(runDir, env);
            await workerExit(worker);
            result = JSON.parse(readFileSync(join(runDir, "results.json"), "utf8"));
          } else {
            result = await executeRun(runDir, env);
          }
        } catch (error) {
          launchError = error;
        }
        assert.equal(existsSync(executed), false, "a malformed host cannot downgrade to standalone execution");
        assert.ok(launchError || (Number.isInteger(result?.exitCode) && result.exitCode !== 0), "partial host environment must abort the launch");
        assert.match(String(launchError?.message ?? result?.error), /registration/iu);
      });
    });
  }

  for (const detached of [false, true]) {
    const fixture = detached ? isolatedDetachedRepository : repository;
    const mode = detached ? "detached" : "foreground";
    for (const rejected of [
      { label: "missing versioned contract", acknowledgement: ({ runId }) => ({ runId, registered: true }) },
      { label: "wrong contract", acknowledgement: (ack) => ({ ...ack, contract: "unrelated" }) },
      { label: "wrong version", acknowledgement: (ack) => ({ ...ack, version: 2 }) },
      { label: "wrong run", acknowledgement: (ack) => ({ ...ack, runId: "test-unrelated" }) },
      { label: "unconfirmed query", queryStatus: 503 },
      { label: "wrong query owner", queryTransform: (run) => ({ ...run, ownerSessionId: "other-owner" }) },
      { label: "non-durable query", queryTransform: (run) => ({ ...run, trackingDurability: "unconfirmed" }) },
      { label: "malformed acknowledgement", rawAcknowledgement: "not JSON" },
      { label: "oversized acknowledgement", rawAcknowledgement: "x".repeat(65_537) },
    ]) {
      test(`host registration ${mode} refuses ${rejected.label} before probes or bootstrap`, async (t) => {
        await fixture(t, async (root, originalEnv) => {
          await registrationHost(async (host) => {
            const env = { ...originalEnv, TERMAL_BASE_URL: host.url };
            await refusedLaunch(root, env, detached, workerExit);
            assert.equal(host.requests.length, 1, "definitive refusal must not repeat registration");
          }, { ...rejected, autoAcknowledge: true });
        });
      });
    }

    test(`host registration ${mode} retries a lost reply with one immutable run`, async (t) => {
      await fixture(t, async (root, originalEnv) => {
        await registrationHost(async (host) => {
          const env = { ...originalEnv, TERMAL_BASE_URL: host.url };
          const executed = join(root, ".git", "lost-reply-stage");
          const runDir = await createRun({ root, detached, full: detached, scope: detached ? undefined : "focused",
            stages: [stage(detached ? "rust-tests" : "focused", `require('node:fs').appendFileSync(${JSON.stringify(executed)}, 'once\\n')`)],
            ...(detached ? { notifyTo: "fixture-coordinator" } : {}),
          }, env);
          assert.equal(host.requests.length, 2);
          assert.equal(host.requests[0].rawBody, host.requests[1].rawBody);
          assert.equal(host.registrations.size, 1);
          assert.equal(existsSync(executed), false);
          if (detached) {
            const worker = await startDetachedRun(runDir, env);
            assert.deepEqual(await workerExit(worker), { code: 0, signal: null });
          } else {
            assert.equal((await executeRun(runDir, env)).state, "passed");
          }
          assert.equal(readFileSync(executed, "utf8"), "once\n");
          assert.equal(host.requests.length, 2, "worker revalidates without another POST");
        }, { autoAcknowledge: true, loseFirstReply: true });
      });
    });

    test(`host registration ${mode} rejects unavailable host without execution`, async (t) => {
      await fixture(t, async (root, originalEnv) => {
        await refusedLaunch(root, { ...originalEnv, TERMAL_BASE_URL: "http://127.0.0.1:0" }, detached, workerExit);
      });
    });

    test(`host registration ${mode} refuses unresolved replies after the bounded retry`, async (t) => {
      await fixture(t, async (root, originalEnv) => {
        await registrationHost(async (host) => {
          await refusedLaunch(root, { ...originalEnv, TERMAL_BASE_URL: host.url }, detached, workerExit);
          assert.equal(host.requests.length, 2);
          assert.equal(host.requests[0].rawBody, host.requests[1].rawBody);
          assert.equal(host.registrations.size, 1);
          assert.equal(host.queries.length, 0);
        }, { autoAcknowledge: true, loseAllReplies: true });
      });
    });

    test(`host registration ${mode} rechecks host visibility before worker preflight`, async (t) => {
      await fixture(t, async (root, originalEnv) => {
        let visible = true;
        await registrationHost(async (host) => {
          const env = { ...originalEnv, TERMAL_BASE_URL: host.url };
          const runDir = await createRun({ root, detached, full: detached, needsCargo: true,
            stages: [stage(detached ? "rust-tests" : "focused")],
            ...(detached ? { notifyTo: "fixture-coordinator" } : {}),
          }, env);
          visible = false;
          if (detached) {
            await assert.rejects(startDetachedRun(runDir, env), /registration/iu);
          } else {
            const result = await executeRun(runDir, env);
            assert.equal(result.state, "failed");
            assert.match(result.error, /registration/iu);
          }
          assert.equal(host.requests.length, 1);
          assert.equal(host.queries.length, 2);
          assert.equal(existsSync(join(root, "target")), false);
          assert.deepEqual(JSON.parse(readFileSync(join(runDir, "results.json"), "utf8")).preflight, []);
          assert.equal(readdirSync(runDir).some((name) => name.startsWith("preflight-")), false);
        }, { autoAcknowledge: true,
          queryTransform: (run) => visible ? run : { ...run, trackingDurability: "unconfirmed" } });
      });
    });

    for (const missing of ["TERMAL_BASE_URL", "TERMAL_SESSION_ID"]) {
      test(`host registration ${mode} rejects partial context missing ${missing}`, async (t) => {
        await fixture(t, async (root, originalEnv) => {
          const env = { ...originalEnv };
          delete env[missing]; // Only this isolated unit fixture's environment.
          await refusedLaunch(root, env, detached, workerExit);
        });
      });
    }

    test(`host registration ${mode} rejects a modified receipt before worker preflight`, async (t) => {
      await fixture(t, async (root, env) => {
        const runDir = await createRun({ root, detached, full: detached, needsCargo: true,
          stages: [stage(detached ? "rust-tests" : "focused")],
          ...(detached ? { notifyTo: "fixture-coordinator" } : {}),
        }, env);
        const requestPath = join(runDir, "request.json");
        const request = JSON.parse(readFileSync(requestPath, "utf8"));
        request.registration.version = 2;
        writeFileSync(requestPath, JSON.stringify(request));
        if (detached) {
          await assert.rejects(startDetachedRun(runDir, env), /registration/iu);
        } else {
          const result = await executeRun(runDir, env);
          assert.equal(result.state, "failed");
          assert.match(result.error, /registration/iu);
        }
        const result = JSON.parse(readFileSync(join(runDir, "results.json"), "utf8"));
        assert.deepEqual(result.preflight, []);
        assert.equal(existsSync(join(root, "target")), false);
        assert.equal(readdirSync(runDir).some((name) => name.startsWith("preflight-")), false);
      });
    });

    test(`host registration ${mode} permits explicit standalone with UNTRACKED provenance`, async (t) => {
      await fixture(t, async (root, hostedEnv) => {
        const env = { ...hostedEnv };
        // Standalone is a deliberate isolated test case, never a real-gate bypass.
        for (const name of ["TERMAL_BASE_URL", "TERMAL_SESSION_ID", "TERMAL_CLI"]) delete env[name];
        const runDir = await createRun({ root, detached, full: detached,
          stages: [stage(detached ? "rust-tests" : "focused")],
        }, env);
        const request = JSON.parse(readFileSync(join(runDir, "request.json"), "utf8"));
        assert.equal(request.registration.state, "untracked");
        assert.match(request.registration.reason, /standalone/iu);
        if (detached) {
          const worker = await startDetachedRun(runDir, env);
          assert.deepEqual(await workerExit(worker), { code: 0, signal: null });
        } else {
          assert.equal((await executeRun(runDir, env)).state, "passed");
        }
      });
    });
  }
}
