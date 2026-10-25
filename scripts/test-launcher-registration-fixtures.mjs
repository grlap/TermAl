// Test-only registration witnesses mounted by scripts/test-launcher.test.mjs.
// Owns the held host acknowledgement and execution barriers; it does not own
// registration, run indexing, credit, or restart policy in the product.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, symlinkSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { basename, join, relative } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createRun, executeRun, startDetachedRun } from "./test-launcher.mjs";
import { registerTestRun, validateRunRegistration } from "./test-run-registration.mjs";

function registrationRootSpelling(t, root, spelling) {
  if (spelling === "original") return root;
  const canonical = realpathSync.native(root);
  if (spelling === "short-name") {
    if (process.platform !== "win32") {
      t.skip("Windows 8.3 short-name witness requires Windows; directory alias is tested separately");
      return;
    }
    const short = execFileSync(process.env.ComSpec ?? "cmd.exe", [
      "/d", "/v:off", "/c", "for %I in (.) do @echo %~fsI",
    ], {
      encoding: "utf8", windowsHide: true,
      cwd: canonical,
    }).trim();
    assert.ok(short, "the native short-path query must return a path");
    if (short.toLowerCase() === canonical.toLowerCase()) {
      t.skip("This test volume supplies no distinct Windows 8.3 spelling; directory alias is tested separately");
      return;
    }
    assert.equal(realpathSync.native(short), canonical);
    t.diagnostic(`Windows 8.3 witness: ${short} -> ${canonical}`);
    return short;
  }
  // Git ignores this alias under .git; it must not enter the source fingerprint.
  const alias = join(root, ".git", "registration directory alias");
  symlinkSync(canonical, alias, process.platform === "win32" ? "junction" : "dir");
  assert.notEqual(alias, canonical, "the witness must use different path spellings");
  assert.equal(realpathSync.native(alias), canonical);
  return alias;
}

function barrier() {
  let release;
  const ready = new Promise((resolve) => { release = resolve; });
  return { ready, release };
}

export async function registrationHost(callback, {
  status = 201, registered = true, autoAcknowledge = false,
  acknowledgement, rawAcknowledgement, queryStatus = 200, queryTransform,
  loseFirstReply = false, loseAllReplies = false,
  errorReason,
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
        response.end(JSON.stringify(errorReason && queryStatus >= 400
          ? { error: errorReason } : { run: queryTransform ? queryTransform(run) : run }));
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
      const responseStatus = typeof status === "function" ? status(record) : status;
      response.writeHead(responseStatus, { "content-type": "application/json" });
      const normalAck = {
        contract: "termal-durable-test-run-registration", version: 1, runId: record.body.runId, registered,
      };
      response.end(rawAcknowledgement ?? JSON.stringify(errorReason && responseStatus >= 400
        ? { error: errorReason } : typeof acknowledgement === "function"
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
  async function pendingRepository(t, callback) {
    // Respect the maintained wrapper when present. A plain, filtered Node
    // invocation needs no environment prefix or writes to the OS temp folder.
    if (process.env.TERMAL_TEST_USER_TEMP) return repository(t, callback);
    let directory = fileURLToPath(new URL("../", import.meta.url));
    for (const component of [".tmp", "launcher-pending"]) {
      directory = join(directory, component);
      try { mkdirSync(directory); } catch (error) { if (error.code !== "EEXIST") throw error; }
      const entry = lstatSync(directory);
      if (entry.isSymbolicLink() || !entry.isDirectory()) {
        throw new Error(`Pending fixture parent must be a plain directory: ${directory}`);
      }
    }
    return repository(t, callback, undefined, directory);
  }

  function pendingRegistration(request) {
    const fields = ["runId", "root", "stages", "notifyTo", "full", "scope", "detached",
      "creatorPid", "owner", "started", "requiredBinaryEnv", "prerequisiteFiles", "needsCargo", "liveEngram"];
    const ordered = (value) => Array.isArray(value) ? value.map(ordered)
      : value && typeof value === "object"
        ? Object.fromEntries(Object.keys(value).sort().map((key) => [key, ordered(value[key])])) : value;
    const immutable = Object.fromEntries(fields.map((key) => [key, request[key] ?? null]));
    return {
      error: "Durability is pending; retry the unchanged original request.",
      contract: "termal-durability-pending", version: 1, code: "durability_pending", outcome: "pending",
      operation: "test_run_registration",
      identity: { runId: request.runId, ownerSessionId: request.owner,
        requestDigest: createHash("sha256").update(JSON.stringify(ordered(immutable))).digest("hex") },
      retry: { mode: "same_request" },
    };
  }

  test("durability pending registration preserves identity and stops before any stage", async (t) => {
    await pendingRepository(t, async (root, env) => {
      let pending;
      let unresolved = true;
      let service;
      await registrationHost(async (host) => {
        service = host;
        let runDir;
        let failure;
        try {
          await createRun({ root, full: false, scope: "focused", needsCargo: true,
            onRunCreated: (directory) => { runDir = directory; },
            stages: [stage("focused", "throw new Error('pending registration admitted execution')")],
          }, { ...env, TERMAL_BASE_URL: host.url });
        } catch (error) { failure = error; }
        assert.ok(failure, "pending is not a registered receipt");
        assertNoExecution(root, runDir);
        assert.equal(host.requests.length, 1, "typed pending has no automatic replay");
        assert.equal(host.queries.length, 0, "pending cannot enter ACK confirmation");
        const original = JSON.parse(readFileSync(join(runDir, "request.json"), "utf8"));
        const result = JSON.parse(readFileSync(join(runDir, "results.json"), "utf8"));
        assert.equal(original.registration, undefined, "no successful receipt is fabricated");
        assert.equal(result.state, "failed", "the launcher does not report a completed check");

        // Explicit resend, not a launcher retry: the host now acknowledges
        // the same row. Neither the immutable request nor the run is replaced.
        unresolved = false;
        const registered = await registerTestRun(original, runDir, { ...env, TERMAL_BASE_URL: host.url });
        assert.equal(registered.registered, true);
        assert.equal(registered.runId, original.runId);
        assert.equal(registered.requestIdentity, pending.identity.requestDigest);
        assert.equal(host.requests.length, 2);
        assert.equal(host.requests[1].rawBody, host.requests[0].rawBody);
        assert.equal(host.registrations.size, 1);
        assertNoExecution(root, runDir);

        // Contract assertions last, after the explicit same-request witness.
        assert.deepEqual(failure.durabilityPending, pending);
        assert.equal(result.registration.state, "pending");
        assert.deepEqual(result.registration.pending, pending);
      }, { autoAcknowledge: true, status: () => unresolved ? 503 : 201, acknowledgement: (ack) => {
        if (!unresolved) return ack;
        // The fixture receives the actual immutable request before replying.
        pending = pendingRegistration(service.registrations.get(ack.runId).request);
        return pending;
      } });
    });
  });

  for (const invalid of ["contract", "version", "code", "outcome", "operation", "runId", "digest", "owner", "retry", "error", "status"]) {
    test(`durability pending registration rejects a mismatched envelope: ${invalid}`, async (t) => {
      await pendingRepository(t, async (root, env) => {
        let service;
        await registrationHost(async (host) => {
          service = host;
          let runDir;
          let failure;
          try {
            await createRun({ root, full: false, scope: "focused", needsCargo: true,
              onRunCreated: (directory) => { runDir = directory; },
              stages: [stage("focused", "throw new Error('malformed pending admitted execution')")],
            }, { ...env, TERMAL_BASE_URL: host.url });
          } catch (error) { failure = error; }
          assert.ok(failure);
          assertNoExecution(root, runDir);
          assert.equal(host.requests.length, 1, "malformed pending does not authorize replay");
          assert.equal(host.queries.length, 0);
          assert.equal(failure.durabilityPending, undefined, "unrelated identities are not this pending operation");
          const result = JSON.parse(readFileSync(join(runDir, "results.json"), "utf8"));
          assert.notEqual(result.registration.state, "pending");
          assert.equal(result.registration.pending, undefined);
        }, { autoAcknowledge: true, status: invalid === "status" ? 201 : 503, acknowledgement: (ack) => {
          const pending = pendingRegistration(service.registrations.get(ack.runId).request);
          switch (invalid) {
            case "contract": pending.contract = "unrelated-contract"; break;
            case "version": pending.version = 2; break;
            case "code": pending.code = "definitely_not_applied"; break;
            case "outcome": pending.outcome = "not_applied"; break;
            case "operation": pending.operation = "named_root_release"; break;
            case "runId": pending.identity.runId = "test-another-run"; break;
            case "digest": pending.identity.requestDigest = "0".repeat(64); break;
            case "owner": pending.identity.ownerSessionId = "another-owner"; break;
            case "retry": pending.retry.mode = "new_request"; break;
            case "error": pending.error = ""; break;
          }
          return pending;
        } });
      });
    });
  }

  test("durability pending registration is not accepted on GET confirmation", async (t) => {
    await pendingRepository(t, async (root, env) => {
      await registrationHost(async (host) => {
        const originalFetch = globalThis.fetch;
        t.mock.method(globalThis, "fetch", async (url, options = {}) => {
          const response = await originalFetch(url, options);
          if ((options.method ?? "GET") !== "GET") return response;
          await response.body.cancel();
          const saved = [...host.registrations.values()][0];
          return new Response(JSON.stringify(pendingRegistration(saved.request)), { status: 503 });
        });
        const failure = await registrationFailure(root, { ...env, TERMAL_BASE_URL: host.url });
        assert.equal(failure.durabilityPending, undefined);
        assert.equal(host.requests.length, 1, "confirmation failure cannot resend POST");
        assert.equal(host.queries.length, 1);
        t.mock.restoreAll();
      }, { autoAcknowledge: true });
    });
  });

  test("durability pending registration retains only bounded supported fields", async (t) => {
    await pendingRepository(t, async (root, env) => {
      let service;
      await registrationHost(async (host) => {
        service = host;
        const failure = await registrationFailure(root, { ...env, TERMAL_BASE_URL: host.url });
        const pending = failure.durabilityPending;
        assert.ok(pending, "the supported envelope survives diagnostic and launcher wrappers");
        assert.deepEqual(Object.keys(pending).sort(),
          ["error", "contract", "version", "code", "outcome", "operation", "identity", "retry"].sort());
        assert.deepEqual(Object.keys(pending.identity).sort(), ["runId", "requestDigest", "ownerSessionId"].sort());
        assert.ok(pending.error.length <= 384);
        assert.doesNotMatch(pending.error, /[\r\n\u0000]/u);
        assert.doesNotMatch(JSON.stringify(pending), /private-field-sentinel/u);
        assert.equal(host.requests.length, 1);
        assert.equal(host.queries.length, 0);
      }, { autoAcknowledge: true, status: 503, acknowledgement: (ack) => {
        const pending = pendingRegistration(service.registrations.get(ack.runId).request);
        return { ...pending, error: `pending\n${"x".repeat(500)}`, extra: "private-field-sentinel",
          identity: { ...pending.identity, extra: "private-field-sentinel" } };
      } });
    });
  });

  async function registrationFailure(root, env) {
    let runDir;
    let failure;
    try {
      await createRun({ root, full: false, scope: "focused", needsCargo: true,
        onRunCreated: (directory) => { runDir = directory; },
        stages: [stage("focused", "throw new Error('registration failure admitted execution')")],
      }, env);
    } catch (error) { failure = error; }
    assert.ok(failure, "registration must fail before any execution");
    assertNoExecution(root, runDir);
    failure.registrationRunId = JSON.parse(readFileSync(join(runDir, "request.json"), "utf8")).runId;
    return failure;
  }

  function assertDiagnostic(failure, method, phase, category, status) {
    assert.match(failure.message, new RegExp(`phase=${method.toLowerCase()}_${phase}(?:;|$)`, "u"));
    assert.match(failure.message, new RegExp(`category=${category}(?:;|$)`, "u"));
    assert.ok(failure.message.includes(`runId=${failure.registrationRunId}`), failure.message);
    assert.match(failure.message, /attempt=[12](?:;|$)/u);
    assert.ok(failure.message.includes(`httpStatus=${status}`), failure.message);
    assert.ok(failure.message.length <= 2048, "the complete diagnostic remains bounded");
    assert.doesNotMatch(failure.message, /[\r\n]/u);
  }

  for (const phase of ["POST", "GET"]) {
    for (const fault of ["abort", "transport"]) {
      test(`registration diagnostics distinguish ${phase} ${fault} before execution`, async (t) => {
        await repository(t, async (root, originalEnv) => {
          await registrationHost(async (host) => {
            const originalFetch = globalThis.fetch;
            let faults = 0;
            t.mock.method(globalThis, "fetch", (url, options = {}) => {
              const method = options.method ?? "GET";
              if (method !== phase) return originalFetch(url, options);
              faults += 1;
              if (fault === "abort") {
                // A real fetch abort, without sleeping through the budget.
                return originalFetch(url, { ...options, signal: AbortSignal.abort() });
              }
              return Promise.reject(new TypeError("fixture transport failure", { cause: { code: "ECONNRESET" } }));
            });
            const failure = await registrationFailure(root, { ...originalEnv, TERMAL_BASE_URL: host.url });
            assert.match(failure.message, new RegExp(`${phase} send ${fault === "abort" ? "aborted" : "transport failure"}`, "u"));
            assertDiagnostic(failure, phase, "headers", fault === "abort" ? "aborted" : "transport_failure", "unavailable");
            assert.equal(faults, phase === "POST" ? 2 : 1, "existing immutable POST retry bound is unchanged");
            if (phase === "GET") assert.equal(host.requests.length, 1, "a query failure cannot retry POST");
            t.mock.restoreAll();
          }, { autoAcknowledge: true });
        });
      });
    }

    test(`registration diagnostics retain ${phase} HTTP status and bounded host reason`, async (t) => {
      await repository(t, async (root, env) => {
        await registrationHost(async (host) => {
          const failure = await registrationFailure(root, { ...env, TERMAL_BASE_URL: host.url });
          assert.match(failure.message, new RegExp(`${phase} HTTP 500`, "u"));
          assert.match(failure.message, /category=deadline; writerProgress=queued/u);
          assertDiagnostic(failure, phase, "body", "http_error", 500);
          assert.ok(failure.message.length < 1400, "error detail remains bounded");
          assert.doesNotMatch(failure.message, /[\r\n]/u);
          assert.equal(host.requests.length, 1, "HTTP refusal is definitive and cannot retry POST");
        }, { autoAcknowledge: true, ...(phase === "POST" ? { status: 500 } : { queryStatus: 500 }),
          errorReason: `phase=durability_wait; category=deadline; writerProgress=queued\n${"x".repeat(2048)}` });
      });
    });

    for (const fault of ["abort", "transport", "malformed", "oversized", "identity"]) {
      test(`registration diagnostics retain ${phase} body or identity failure without admission: ${fault}`, async (t) => {
        await repository(t, async (root, env) => {
          await registrationHost(async (host) => {
            const originalFetch = globalThis.fetch;
            let faults = 0;
            t.mock.method(globalThis, "fetch", async (url, options = {}) => {
              const response = await originalFetch(url, options);
              if ((options.method ?? "GET") !== phase) return response;
              faults += 1;
              if (fault === "identity") {
                const body = await response.json();
                if (phase === "POST") body.runId = "test-wrong-registration-identity";
                else body.run.runId = "test-wrong-registration-identity";
                return new Response(JSON.stringify(body), { status: response.status });
              }
              await response.body.cancel();
              if (fault === "malformed") return new Response("{", { status: response.status });
              if (fault === "oversized") return new Response("x".repeat(65_537), { status: response.status });
              // Headers came from the real private server; fail the native
              // body stream at the next boundary without waiting out 8 seconds.
              const body = new ReadableStream({ start(controller) {
                controller.error(fault === "abort" ? new DOMException("fixture body aborted", "AbortError")
                  : new TypeError("fixture body transport", { cause: { code: "ECONNRESET" } }));
              } });
              return new Response(body, { status: response.status });
            });
            const failure = await registrationFailure(root, { ...env, TERMAL_BASE_URL: host.url });
            const category = { abort: "aborted", transport: "transport_failure", malformed: "malformed_response",
              oversized: "oversized_response", identity: "identity_mismatch" }[fault];
            assertDiagnostic(failure, phase, fault === "identity" ? (phase === "POST" ? "validation" : "identity") : "body",
              category, phase === "POST" ? 201 : 200);
            const attempts = phase === "POST" && ["abort", "transport"].includes(fault) ? 2 : 1;
            assert.equal(faults, attempts, "only existing ambiguous POST loss may retry");
            assert.equal(host.requests.length, attempts, "a failed GET never repeats POST");
            assert.ok(host.requests.every((entry) => entry.rawBody === host.requests[0].rawBody), "retries retain exact request bytes");
            assert.equal(host.queries.length, phase === "POST" ? 0 : 1);
            t.mock.restoreAll();
          }, { autoAcknowledge: true });
        });
      });
    }

    for (const fault of ["abort", "transport", "malformed", "oversized", "cancel"]) {
      test(`registration diagnostics keep known ${phase} HTTP status when error body fails: ${fault}`, async (t) => {
        await repository(t, async (root, env) => {
          await registrationHost(async (host) => {
            const originalFetch = globalThis.fetch;
            t.mock.method(globalThis, "fetch", async (url, options = {}) => {
              const response = await originalFetch(url, options);
              if ((options.method ?? "GET") !== phase) return response;
              await response.body.cancel();
              let body;
              if (fault === "abort" || fault === "transport") {
                body = new ReadableStream({ start(controller) {
                  controller.error(fault === "abort" ? new DOMException("fixture error body aborted", "AbortError")
                    : new TypeError("fixture error body transport", { cause: { code: "ECONNRESET" } }));
                } });
              } else if (fault === "cancel") {
                // Early bounded-read exit invokes cancel; its failure must not
                // replace the already received HTTP refusal.
                body = new ReadableStream({ start(controller) { controller.enqueue(new Uint8Array(65_537)); },
                  cancel() { throw new TypeError("fixture cancel failed"); } });
              } else body = fault === "malformed" ? "{" : "x".repeat(65_537);
              return new Response(body, { status: response.status });
            });
            const failure = await registrationFailure(root, { ...env, TERMAL_BASE_URL: host.url });
            assertDiagnostic(failure, phase, "body", "http_error", 500);
            assert.match(failure.message, new RegExp(`${phase} HTTP 500`, "u"));
            assert.match(failure.message, /host reason unavailable/u);
            assert.equal(host.requests.length, 1, "known HTTP refusal is never retried, even when reading or cancelling fails");
            assert.equal(host.queries.length, phase === "POST" ? 0 : 1);
            t.mock.restoreAll();
          }, { autoAcknowledge: true, ...(phase === "POST" ? { status: 500 } : { queryStatus: 500 }),
            errorReason: "fixture host refusal" });
        });
      });
    }
  }

  for (const detached of [false, true]) {
    const fixture = detached ? isolatedDetachedRepository : repository;
    const mode = detached ? "detached" : "foreground";
    for (const field of ["runDir", "worktree"]) {
      for (const spelling of ["directory-alias", "short-name"]) {
      test(`host registration ${mode} accepts ${spelling} in query ${field}`, async (t) => {
        await fixture(t, async (root, originalEnv) => {
          const canonical = realpathSync.native(root);
          const alias = registrationRootSpelling(t, root, spelling);
          if (!alias) return;
          const executed = join(root, ".git", "registration-path-executed");
          await registrationHost(async (host) => {
            const env = { ...originalEnv, TERMAL_BASE_URL: host.url };
            const runDir = await createRun({
              root: field === "worktree" ? alias : canonical,
              detached, full: detached, scope: detached ? undefined : "focused",
              stages: [stage("registration-path-witness", `require('node:fs').appendFileSync(${JSON.stringify(executed)}, 'once\\n')`)],
              ...(detached ? { notifyTo: "fixture-coordinator" } : {}),
            }, env);
            assert.equal(host.requests.length, 1);
            assert.equal(host.queries.length, 1);
            assert.equal(existsSync(executed), false);
            if (detached) {
              assert.deepEqual(await workerExit(await startDetachedRun(runDir, env)), { code: 0, signal: null });
            } else {
              assert.equal((await executeRun(runDir, env)).state, "passed");
            }
            assert.equal(host.requests.length, 1, "fresh worker validation must not repeat the POST");
            assert.equal(host.queries.length, 2);
            assert.equal(readFileSync(executed, "utf8"), "once\n");
          }, { autoAcknowledge: true, queryTransform: (run) => ({
            ...run,
            [field]: field === "worktree" ? canonical
              : join(alias, relative(canonical, realpathSync.native(run.runDir))),
          }) });
        });
      });
      }

      for (const unavailable of [false, true]) {
        test(`host registration ${mode} refuses a ${unavailable ? "missing" : "different existing"} query ${field}`, async (t) => {
          await fixture(t, async (root, originalEnv) => {
            const different = join(root, "different registration directory");
            if (!unavailable) mkdirSync(different);
            await registrationHost(async (host) => {
              await refusedLaunch(root, { ...originalEnv, TERMAL_BASE_URL: host.url }, detached, workerExit);
              assert.equal(host.requests.length, 1);
              assert.equal(host.queries.length, 1);
            }, { autoAcknowledge: true, queryTransform: (run) => ({ ...run, [field]: different }) });
          });
        });
      }
    }

    for (const spelling of ["directory-alias", "short-name"]) {
    test(`host registration ${mode} accepts ${spelling} in the worker receipt`, async (t) => {
      await fixture(t, async (root, originalEnv) => {
        const canonical = realpathSync.native(root);
        const alias = registrationRootSpelling(t, root, spelling);
        if (!alias) return;
        const executed = join(root, ".git", "registration-receipt-executed");
        await registrationHost(async (host) => {
          const env = { ...originalEnv, TERMAL_BASE_URL: host.url };
          const runDir = await createRun({ root, detached, full: detached, scope: detached ? undefined : "focused",
            stages: [stage("registration-path-witness", `require('node:fs').appendFileSync(${JSON.stringify(executed)}, 'once\\n')`)],
            ...(detached ? { notifyTo: "fixture-coordinator" } : {}),
          }, env);
          const requestPath = join(runDir, "request.json");
          const request = JSON.parse(readFileSync(requestPath, "utf8"));
          const originalReceipt = request.registration;
          request.registration = { ...originalReceipt,
            runDirectory: join(alias, relative(canonical, realpathSync.native(runDir))),
          };
          assert.notEqual(request.registration.runDirectory, originalReceipt.runDirectory);
          assert.equal(realpathSync.native(request.registration.runDirectory), realpathSync.native(runDir));
          writeFileSync(requestPath, JSON.stringify(request));
          assert.equal(existsSync(executed), false);
          if (detached) {
            assert.deepEqual(await workerExit(await startDetachedRun(runDir, env)), { code: 0, signal: null });
          } else {
            assert.equal((await executeRun(runDir, env)).state, "passed");
          }
          assert.equal(host.requests.length, 1);
          assert.equal(host.queries.length, 2, "the alias must still cross fresh host validation");
          assert.equal(readFileSync(executed, "utf8"), "once\n");
        }, { autoAcknowledge: true });
      });
    });
    }

    for (const unavailable of [false, true]) {
      test(`host registration ${mode} refuses a ${unavailable ? "missing" : "different existing"} worker receipt path`, async (t) => {
        await fixture(t, async (root, originalEnv) => {
          await registrationHost(async (host) => {
            const env = { ...originalEnv, TERMAL_BASE_URL: host.url };
            const executed = join(root, "stage-after-registration-error");
            const runDir = await createRun({ root, detached, full: detached, needsCargo: true,
              stages: [stage("registration-path-witness", `require('node:fs').writeFileSync(${JSON.stringify(executed)}, 'ran')`)],
              ...(detached ? { notifyTo: "fixture-coordinator" } : {}),
            }, env);
            const different = join(root, "different receipt directory");
            if (!unavailable) mkdirSync(different);
            const requestPath = join(runDir, "request.json");
            const request = JSON.parse(readFileSync(requestPath, "utf8"));
            request.registration = { ...request.registration, runDirectory: different };
            writeFileSync(requestPath, JSON.stringify(request));
            if (detached) {
              await assert.rejects(startDetachedRun(runDir, env), /registration/iu);
            } else {
              const result = await executeRun(runDir, env);
              assert.equal(result.state, "failed");
              assert.match(result.error, /registration/iu);
            }
            assert.equal(host.requests.length, 1);
            assert.equal(host.queries.length, 1, "the bad receipt must be refused before a fresh query");
            assert.equal(existsSync(executed), false);
            assert.equal(existsSync(join(root, "target")), false);
            const result = JSON.parse(readFileSync(join(runDir, "results.json"), "utf8"));
            assert.deepEqual(result.preflight, []);
            assert.ok(result.stages.every(({ state }) => state === "unrun"));
          }, { autoAcknowledge: true });
        });
      });
    }
  }

  test("host registration refuses two matching unavailable path spellings at worker validation", async (t) => {
    await repository(t, async (root, originalEnv) => {
      let unavailable;
      await registrationHost(async (host) => {
        const env = { ...originalEnv, TERMAL_BASE_URL: host.url };
        const runDir = await createRun({ root, needsCargo: true,
          stages: [stage("registration-path-witness")],
        }, env);
        const request = JSON.parse(readFileSync(join(runDir, "request.json"), "utf8"));
        unavailable = join(root, "missing registration directory");
        request.registration = { ...request.registration, runDirectory: unavailable };
        await assert.rejects(validateRunRegistration(request, env, unavailable), /registration/iu);
        assert.equal(host.requests.length, 1);
        assert.equal(existsSync(join(root, "target")), false);
        const result = JSON.parse(readFileSync(join(runDir, "results.json"), "utf8"));
        assert.deepEqual(result.preflight, []);
        assert.ok(result.stages.every(({ state }) => state === "unrun"));
      }, { autoAcknowledge: true,
        queryTransform: (run) => unavailable ? { ...run, runDir: unavailable } : run });
    });
  });

  for (const detached of [false, true]) {
    for (const spelling of ["original", "directory-alias", "short-name"]) {
    test(`host registration is acknowledged before the first ${detached ? "detached full" : "foreground focused"} stage${spelling === "original" ? "" : ` through ${spelling}`}`, async (t) => {
      const fixture = detached ? isolatedDetachedRepository : repository;
      await fixture(t, async (root, originalEnv) => {
        root = registrationRootSpelling(t, root, spelling);
        if (!root) return;
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
            assert.deepEqual(result.preflight.map(({ name, code }) => ({ name, code })), [
              ...(detached ? [{ name: "ui-dependencies", code: 0 }] : []),
              { name: "cargo", code: 0 },
            ]);
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
            assert.equal(realpathSync.native(outcome.runDir), realpathSync.native(runDir));
            assert.equal(readFileSync(executed, "utf8"), "once\n");
            assert.equal(host.requests.length, 1, "worker handoff must not register a second run");
          } finally {
            host.acknowledge();
            await finished;
          }
        });
      });
    });
    }

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
