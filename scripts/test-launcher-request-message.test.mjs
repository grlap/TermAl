// Exercises registration wire and acknowledgement behavior using private HTTP
// fixtures. No live host, native verification credit or product gate is minted.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdtempSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { execFileSync, spawn } from "node:child_process";
import { createRun, executeRun, startDetachedRun, summarize } from "./test-launcher.mjs";
import { registerTestRun, validateRunRegistration, validateRequestMessageId } from "./test-run-registration.mjs";
import { registrationHost } from "./test-launcher-registration-fixtures.mjs";
import { fixtureSourceEnvironment } from "./test-launcher-source-owner-fixture.mjs";
import { removeFixtureOnPass } from "./test-fixture-cleanup.mjs";
import { isolatedGitEnvironment } from "./git-environment.mjs";
import { testTempDirectory } from "./test-temp-root.mjs";

const selector = "mailbox-message-11111111-1111-4111-8111-111111111111";
const other = "relay-message-QWJjMTIz_-";
const project = fileURLToPath(new URL("../", import.meta.url));
function fixture(t, { full = false, detached = false } = {}) {
  const parent = testTempDirectory();
  const root = mkdtempSync(join(parent, "request-message-"));
  removeFixtureOnPass(t, root);
  const runDirectory = join(root, "run");
  mkdirSync(runDirectory);
  const request = { runId: "test-request-message", root, owner: "fixture-owner", creatorPid: process.pid,
    full, detached, input: "commit", started: "2026-11-12T01:00:00Z", stages: [{ name: full ? "cargo-check" : "focused" }] };
  return { root, runDirectory, request };
}
function saveRequest(f) { writeFileSync(join(f.runDirectory, "request.json"), JSON.stringify(f.request)); }
function env(host) {
  return fixtureSourceEnvironment({
    ...process.env, TERMAL_SESSION_ID: "fixture-owner", TERMAL_BASE_URL: host.url, TERMAL_CLI: process.execPath,
  });
}

function initializeRepository(root) {
  const options = { cwd: root, env: isolatedGitEnvironment(process.env), windowsHide: true, stdio: "pipe" };
  execFileSync("git", ["init", "--quiet"], options);
  writeFileSync(join(root, ".gitignore"), "/run\n");
  execFileSync("git", ["add", "--", ".gitignore"], options);
  execFileSync("git", ["-c", "user.name=TermAl fixture", "-c", "user.email=fixture@example.invalid",
    "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture"], options);
}

test("selector grammar is exact and never trims or accepts an explicit empty/null value", () => {
  for (const good of [selector, other, `relay-message-${"a".repeat(1024)}`]) assert.equal(validateRequestMessageId(good), good);
  for (const bad of [undefined, null, "", ` ${selector}`, `${selector}\n`, selector.toUpperCase(),
    "mailbox-message-AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA", "relay-message-", `relay-message-${"a".repeat(1025)}`, "mailbox-message-short"])
    assert.throws(() => validateRequestMessageId(bad), /malformed requestMessageId/u);
});

for (const full of [false, true]) for (const detached of [false, true]) {
  test(`selected ${full ? "full" : "focused"} ${detached ? "detached" : "foreground"} request survives POST and exact GET`, async (t) => {
    const f = fixture(t, { full, detached });
    f.request.requestMessageId = selector;
    saveRequest(f);
    await registrationHost(async (host) => {
      const registration = await registerTestRun(f.request, f.runDirectory, env(host));
      assert.equal(host.requests[0].body.requestMessageId, selector);
      assert.equal(host.requests[0].body.runDirectory, f.runDirectory);
      await validateRunRegistration({ ...f.request, registration }, env(host), f.runDirectory);
      assert.equal(host.requests.length, 1, "a confirmed registration is not appended again");
    }, { autoAcknowledge: true });
  });
}

test("ordinary launch identity remains historical and selecting a different message changes it", async (t) => {
  const f = fixture(t);
  await registrationHost(async (host) => {
    const identity = async () => {
      // Each variant models a fresh host registration. Reusing a durable run's
      // identity after editing its artifact is tested as a refusal below.
      host.registrations.delete(f.request.runId);
      saveRequest(f);
      return (await registerTestRun(f.request, f.runDirectory, env(host))).requestIdentity;
    };
    const legacy = await identity();
    const names = ["runId", "root", "stages", "notifyTo", "full", "scope", "detached", "creatorPid", "owner", "started", "requiredBinaryEnv", "prerequisiteFiles", "needsCargo", "liveEngram"];
    const historical = Object.fromEntries(names.map(name => [name, f.request[name] ?? null]));
    historical.input = "commit";
    const ordered = value => Array.isArray(value) ? value.map(ordered) : value && typeof value === "object"
      ? Object.fromEntries(Object.keys(value).sort().map(key => [key, ordered(value[key])])) : value;
    assert.equal(legacy, createHash("sha256").update(JSON.stringify(ordered(historical))).digest("hex"));
    f.request.requestMessageId = selector;
    const first = await identity();
    assert.notEqual(first, legacy);
    f.request.requestMessageId = other;
    assert.notEqual(await identity(), first);
    delete f.request.requestMessageId;
    assert.equal(await identity(), legacy);
    assert.deepEqual(Object.keys(host.requests[0].body).sort(), ["runDirectory", "runId"]);
  }, { autoAcknowledge: true });
});

for (const echo of [undefined, other]) {
  test(`registration refuses ${echo === undefined ? "missing" : "changed"} selector echo`, async (t) => {
    const f = fixture(t);
    f.request.requestMessageId = selector;
    saveRequest(f);
    await registrationHost(async (host) => {
      await assert.rejects(registerTestRun(f.request, f.runDirectory, env(host)), /exact durable run/u);
      assert.equal(host.requests.length, 1);
    }, { autoAcknowledge: true, queryTransform: run => ({ ...run, requestMessageId: echo }) });
  });
}

test("standalone selection is refused before source probes or run creation", async (t) => {
  const f = fixture(t);
  await assert.rejects(createRun({ root: f.root, stages: [], requestMessageId: selector }, {}), /native TermAl host/u);
  f.request.requestMessageId = selector;
  await assert.rejects(registerTestRun(f.request, f.runDirectory, {}), /native TermAl host/u);
});

test("CLI rejects duplicate, malformed and missing selectors before any stage", async () => {
  const script = fileURLToPath(new URL("./test-launcher.mjs", import.meta.url));
  for (const options of [["--request-message-id"], ["--request-message-id", "invalid"],
    ["--request-message-id", selector, "--request-message-id", other]]) {
    const result = await new Promise((resolve, reject) => {
      const child = spawn(process.execPath, [script, "focused", ...options], {
        cwd: project, windowsHide: true, env: { ...process.env, TERMAL_BASE_URL: "http://127.0.0.1:0" },
        stdio: ["ignore", "pipe", "pipe"],
      });
      let output = "";
      child.stdout.on("data", data => { output += data; });
      child.stderr.on("data", data => { output += data; });
      child.once("error", reject);
      child.once("close", code => resolve({ code, output }));
    });
    assert.notEqual(result.code, 0);
    assert.match(result.output, /request-message-id|requestMessageId/u);
    assert.doesNotMatch(result.output, /^RUN |^PASS /mu);
  }
});

for (const full of [false, true]) for (const detached of [false, true]) {
  test(`real ${full ? "full" : "focused"} ${detached ? "detached" : "foreground"} worker refuses a changed selector echo before stages`, async (t) => {
    const f = fixture(t);
    initializeRepository(f.root);
    const marker = join(f.root, ".git", "must-not-execute");
    let confirmed = false;
    await registrationHost(async (host) => {
      const runEnv = env(host);
      const runDir = await createRun({ root: f.root, full, detached, input: "commit", requestMessageId: selector,
        ...(detached ? { notifyTo: "fixture-coordinator" } : {}),
        stages: [{ name: "focused", command: process.execPath,
          args: ["-e", `require('node:fs').writeFileSync(${JSON.stringify(marker)}, 'executed')`] }],
      }, runEnv);
      const saved = JSON.parse(readFileSync(join(runDir, "request.json"), "utf8"));
      assert.equal(saved.requestMessageId, selector);
      assert.equal(saved.full, full);
      assert.equal(saved.detached, detached);
      assert.equal(saved.registration.state, "registered");
      confirmed = true;
      if (detached) {
        await assert.rejects(startDetachedRun(runDir, runEnv), /registration|exact durable run/iu);
      } else {
        const result = await executeRun(runDir, runEnv);
        assert.equal(result.state, "failed");
        assert.match(result.error, /exact durable run/u);
      }
      assert.equal(existsSync(marker), false);
      assert.equal(host.requests.length, 1, "a worker rechecks the original registration, without a replacement POST");
      const result = JSON.parse(readFileSync(join(runDir, "results.json"), "utf8"));
      assert.ok(result.stages.every(stage => stage.state === "unrun"));
    }, { autoAcknowledge: true, queryTransform: run => confirmed ? { ...run, requestMessageId: other } : run });
  });
}

test("a selected focused request executes only after the exact original ACK", async (t) => {
  const f = fixture(t);
  initializeRepository(f.root);
  const marker = join(f.root, ".git", "selected-stage-executed");
  await registrationHost(async (host) => {
    const runEnv = env(host);
    const runDir = await createRun({ root: f.root, input: "commit", requestMessageId: selector,
      stages: [{ name: "focused", command: process.execPath,
        args: ["-e", `require('node:fs').appendFileSync(${JSON.stringify(marker)}, 'once\\n')`] }],
    }, runEnv);
    assert.equal(existsSync(marker), false);
    assert.equal((await executeRun(runDir, runEnv)).state, "passed");
    assert.equal(readFileSync(marker, "utf8"), "once\n");
    assert.equal(host.requests.length, 1);
    assert.equal(host.queries.length, 2);
  }, { autoAcknowledge: true });
});

test("editing a selected request invalidates its saved ACK instead of acquiring a later request", async (t) => {
  const f = fixture(t);
  f.request.requestMessageId = selector;
  saveRequest(f);
  await registrationHost(async (host) => {
    const registration = await registerTestRun(f.request, f.runDirectory, env(host));
    await assert.rejects(validateRunRegistration({ ...f.request, requestMessageId: other, registration }, env(host), f.runDirectory),
      /matching acknowledged registration/u);
    assert.equal(host.requests.length, 1);
    assert.equal(host.queries.length, 1, "a changed request cannot reuse the acknowledged identity");
  }, { autoAcknowledge: true });
});

// The host's typed refusal while the project's Engram store is in repair.
const inRepair = { error: "test registration held-claim read: the project's Engram store is in repair (engram_store_transient)",
  code: "engram_store_transient" };

test("a store in repair is waited out with printed pauses, then the unchanged request registers once and the run executes", async (t) => {
  const f = fixture(t);
  initializeRepository(f.root);
  const marker = join(f.root, ".git", "transient-stage-executed");
  const pauses = [];
  const log = t.mock.method(console, "log");
  let service;
  await registrationHost(async (host) => {
    service = host;
    const runDir = await createRun({ root: f.root, input: "commit",
      registrationWait: { pause: async (ms) => { pauses.push(ms); } },
      stages: [{ name: "focused", command: process.execPath,
        args: ["-e", `require('node:fs').appendFileSync(${JSON.stringify(marker)}, 'once\\n')`] }],
    }, env(host));
    assert.deepEqual(pauses, [10_000, 20_000]);
    const waits = log.mock.calls.map(call => String(call.arguments[0])).filter(line => line.startsWith("registration waiting:"));
    assert.equal(waits.length, 2);
    assert.match(waits[0], /Engram store is in repair \(engram_store_transient\).*in 10 s/u);
    assert.equal(host.requests.length, 3);
    assert.ok(host.requests.every(record => record.rawBody === host.requests[0].rawBody), "every send is the unchanged request");
    assert.equal(JSON.parse(readFileSync(join(runDir, "request.json"), "utf8")).registration.state, "registered");
    assert.equal((await executeRun(runDir, env(host))).state, "passed");
    assert.equal(readFileSync(marker, "utf8"), "once\n");
    assert.equal(host.requests.length, 3, "the run registers exactly once");
  }, { autoAcknowledge: true, status: () => service.requests.length <= 2 ? 503 : 201,
    acknowledgement: (ack) => service.requests.length <= 2 ? inRepair : ack });
});

test("a store still in repair at the wait bound ends with a named refusal and no tests", async (t) => {
  const f = fixture(t);
  initializeRepository(f.root);
  const marker = join(f.root, ".git", "must-not-execute");
  const pauses = [];
  let runDir;
  await registrationHost(async (host) => {
    await assert.rejects(createRun({ root: f.root, input: "commit", onRunCreated: (directory) => { runDir = directory; },
      registrationWait: { boundMs: 70_000, pause: async (ms) => { pauses.push(ms); } },
      stages: [{ name: "focused", command: process.execPath,
        args: ["-e", `require('node:fs').writeFileSync(${JSON.stringify(marker)}, 'executed')`] }],
    }, env(host)), (error) => {
      assert.match(error.message, /Engram store stayed in repair \(engram_store_transient\) through 4 sends and 70 s of pauses/u);
      assert.match(error.message, /no tests started; rerun once the store is ready/u);
      assert.doesNotMatch(error.message, /named claim lapsed/u);
      return true;
    });
    assert.deepEqual(pauses, [10_000, 20_000, 40_000]);
    assert.equal(host.requests.length, 4);
  }, { autoAcknowledge: true, status: 503, acknowledgement: () => inRepair });
  assert.equal(existsSync(marker), false);
  const result = JSON.parse(readFileSync(join(runDir, "results.json"), "utf8"));
  assert.equal(result.state, "failed");
  assert.equal(result.registration.state, "failed");
  assert.match(result.error, /stayed in repair/u);
  assert.ok(result.stages.every(stage => stage.state === "unrun"));
  // The summary names the store as the cause and the correction for credit,
  // without blaming the claim or the root.
  const summary = await summarize(runDir);
  assert.match(summary, /verification: none; no tests ran: the project's Engram store stayed in repair; once it is ready, register at start with --input commit in the claim's named root/u);
  assert.doesNotMatch(summary, /refused or could not be confirmed|not registered at start/u);
});

for (const [label, status, body] of [
  ["a named host refusal", 502, { error: "test registration held-claim read: Engram held-claim reader failed: work_invalid" }],
  ["a 503 with another code", 503, { error: "fixture unavailable", code: "other_code" }],
]) {
  test(`${label} is not waited on or resent`, async (t) => {
    const f = fixture(t);
    saveRequest(f);
    const pauses = [];
    await registrationHost(async (host) => {
      await assert.rejects(registerTestRun(f.request, f.runDirectory, env(host), { pause: async (ms) => { pauses.push(ms); } }),
        (error) => {
          assert.match(error.message, /host refused registration/u);
          assert.match(error.message, new RegExp(body.error.slice(0, 40), "u"));
          assert.equal(error.storeTransient, undefined);
          return true;
        });
      assert.deepEqual(pauses, []);
      assert.equal(host.requests.length, 1);
    }, { autoAcknowledge: true, status, acknowledgement: () => body });
  });
}

test("a lost selected-request ACK replays the same selector and original registration bytes", async (t) => {
  const f = fixture(t);
  f.request.requestMessageId = selector;
  saveRequest(f);
  await registrationHost(async (host) => {
    const receipt = await registerTestRun(f.request, f.runDirectory, env(host));
    assert.equal(receipt.state, "registered");
    assert.equal(host.requests.length, 2);
    assert.equal(host.requests[1].rawBody, host.requests[0].rawBody);
    assert.equal(host.requests[0].body.requestMessageId, selector);
    assert.equal(host.registrations.size, 1);
    assert.equal(host.queries.length, 1);
    await validateRunRegistration({ ...f.request, registration: receipt }, env(host), f.runDirectory);
    assert.equal(host.requests.length, 2, "confirmation does not replace the logical request");
  }, { autoAcknowledge: true, loseFirstReply: true });
});

// A passing focused run with committed input whose verification credit the
// summary explains. Credit rests on registration at START, so the summary
// names why a run earned none and that only a new run registered that way can.
async function creditRun(t, runEnv, input = "commit") {
  const f = fixture(t);
  initializeRepository(f.root);
  const runDir = await createRun({ root: f.root, input,
    stages: [{ name: "focused", command: process.execPath, args: ["-e", "process.exit(0)"] }] }, runEnv);
  assert.equal((await executeRun(runDir, runEnv)).state, "passed");
  return await summarize(runDir);
}
const correction = "register at start with --input commit in the claim's named root";

const hostlessEnv = () => {
  const hostless = { ...process.env };
  for (const name of ["TERMAL_BASE_URL", "TERMAL_SESSION_ID", "TERMAL_CLI"]) delete hostless[name];
  return fixtureSourceEnvironment(hostless);
};

test("a hostless passing run says why it earned no credit and how to get it", async (t) => {
  const summary = await creditRun(t, hostlessEnv());
  assert.match(summary, /verification: none; not registered at start for credit: it had no TermAl host or session/u);
  assert.ok(summary.includes(correction), summary);
});

// The host mirrors a run's verification record into the run directory after
// the launcher exits. The summary is the documented read-only route to it.
async function failedRunWithRecordState(t, state) {
  const f = fixture(t);
  initializeRepository(f.root);
  const runEnv = hostlessEnv();
  const runDir = await createRun({ root: f.root, input: "commit",
    stages: [{ name: "focused", command: process.execPath, args: ["-e", "process.exit(1)"] }] }, runEnv);
  assert.equal((await executeRun(runDir, runEnv)).state, "failed");
  writeFileSync(join(runDir, "engram-verification-record-state.json"), JSON.stringify(state));
  return await summarize(runDir);
}
const failedReceipt = { operation: "verification_record", producer: "termal-host",
  verifications: [{ stage: "focused", record_id: "record-0001" }, { stage: "focused", record_id: "record-0002" }],
  run_outcome: "failed" };

test("a failed run's settled verification record is named in its summary with its outcome", async (t) => {
  const summary = await failedRunWithRecordState(t, { state: "settled", receipt: failedReceipt });
  assert.match(summary, /^verification: settled; Engram record\(s\) record-0001 record-0002; run outcome failed$/mu);
});

test("a received but unsettled verification record is named in the summary as not yet settled", async (t) => {
  const summary = await failedRunWithRecordState(t, { state: "received", receipt: failedReceipt });
  assert.match(summary,
    /^verification: received, not yet settled; Engram record\(s\) record-0001 record-0002; run outcome failed$/mu);
});

test("a hostless plain-input run keeps only its input: plain explanation", async (t) => {
  const summary = await creditRun(t, hostlessEnv(), "plain");
  assert.match(summary, /^input: plain; no committed-input verification: /mu);
  assert.doesNotMatch(summary, /verification: none;/u);
});

test("a registered run the host bound to no claim says so; a bound one, or one the host did not report, names no cause", async (t) => {
  // The host's registration view reports `bound` (src/test_run_registration_query.rs);
  // a host built before it did reports nothing.
  const withoutBound = (run) => {
    const { bound, ...rest } = run;
    return rest;
  };
  for (const [label, transform, expected] of [
    ["unbound", (run) => ({ ...run, bound: false }), true],
    ["bound", (run) => ({ ...run, bound: true }), false],
    ["not reported", withoutBound, false],
  ]) {
    await registrationHost(async (host) => {
      const summary = await creditRun(t, env(host));
      assert.equal(summary.includes("it was not bound to a claim"), expected, `${label}: ${summary}`);
      assert.equal(summary.includes("verification: none;"), expected, `${label}: ${summary}`);
    }, { autoAcknowledge: true, queryTransform: (run) => run && transform(run) });
  }
});

// Every way a committed-input launch stops before its first stage because it
// was not registered for credit, each named with the correction. Refused
// before it has a run directory, the launch has no summary and its refusal
// names the correction; refused at or after registration, the refusal and
// the run's summary both do. The host may hold a registration it acknowledged
// but could not confirm, so such a summary says only that no tests ran, never
// that the run was not registered. A plain-input launch keeps its own
// explanation. Hostless and unbound runs that ran are covered above. A write
// failure of the first records, just after the run directory is made, takes
// the same refusal path as the directory itself and cannot be induced here
// without replacing the file system.
const unregisteredSummary = /verification: none; no tests ran: the registration was refused or could not be confirmed/u;
// A registration whose durability stays pending through its one identical
// replay: the host may still hold it, so its summary says only that no tests
// ran and that credit needs a new run. The host answers with the typed
// pending reply for the immutable request it read from the run directory.
function pendingRefusal() {
  let created;
  const fields = ["runId", "root", "stages", "notifyTo", "full", "scope", "detached",
    "creatorPid", "owner", "started", "requiredBinaryEnv", "prerequisiteFiles", "needsCargo", "liveEngram"];
  const ordered = (value) => Array.isArray(value) ? value.map(ordered)
    : value && typeof value === "object"
      ? Object.fromEntries(Object.keys(value).sort().map((key) => [key, ordered(value[key])])) : value;
  const pending = () => {
    const request = JSON.parse(readFileSync(join(created, "request.json"), "utf8"));
    const immutable = Object.fromEntries(fields.map((key) => [key, request[key] ?? null]));
    if (request.input !== undefined) immutable.input = request.input;
    if (request.sourceOwner !== undefined) immutable.sourceOwner = request.sourceOwner;
    return {
      error: "Durability is pending; retry the unchanged original request.",
      contract: "termal-durability-pending", version: 1, code: "durability_pending", outcome: "pending",
      operation: "test_run_registration",
      identity: { runId: request.runId, ownerSessionId: request.owner,
        requestDigest: createHash("sha256").update(JSON.stringify(ordered(immutable))).digest("hex") },
      retry: { mode: "same_request" },
    };
  };
  return {
    label: "a registration whose durability stays pending",
    afterCreated: (directory) => { created = directory; },
    registration: { status: 503, acknowledgement: pending },
    refusal: /durability is pending/iu,
    summary: /verification: none; no tests ran: the registration was not confirmed and the host may still hold it; for credit, start a new run: /u,
  };
}
const launchRefusals = [
  { label: "a selected request with no host", options: { requestMessageId: selector },
    refusal: /selected test requests require their native TermAl host/u },
  { label: "a malformed session id", env: { TERMAL_SESSION_ID: "bad session" },
    refusal: /registration error: missing or malformed TERMAL_SESSION_ID/u },
  { label: "a malformed host URL", env: { TERMAL_SESSION_ID: "fixture-owner", TERMAL_BASE_URL: "not a url" },
    refusal: /registration error: missing or malformed TERMAL_BASE_URL/u },
  { label: "a host URL with credentials", env: { TERMAL_SESSION_ID: "fixture-owner", TERMAL_BASE_URL: "http://user:pw@127.0.0.1:9/" },
    refusal: /registration error: unsupported TermAl host URL/u },
  { label: "a partial host identity under --notify", env: { TERMAL_SESSION_ID: "fixture-owner" }, options: { notifyTo: "coordinator" },
    refusal: /registration error: missing or malformed TERMAL_BASE_URL/u },
  // No host at all: a notification needs one, and so does credit.
  { label: "a hostless launch under --notify", options: { notifyTo: "coordinator" },
    refusal: /notification sender/u },
  { label: "an unreachable host", host: "down",
    refusal: /source owner metadata unavailable \(transport_failure; ECONNREFUSED\); no tests started/u },
  { label: "an unreachable host under automatic input", host: "down", options: { input: undefined, automaticInput: true },
    refusal: /source owner metadata unavailable/u },
  { label: "a host refusing source-owner metadata", host: async () => new Response("{}", { status: 503 }),
    refusal: /source owner metadata refused \(503\); no tests started/u },
  { label: "malformed source-owner metadata", host: async () => new Response("{}", { status: 200 }),
    refusal: /source owner metadata is malformed or unsupported; no tests started/u },
  { label: "a relative Engram source binary", env: { ENGRAM_SOURCE_BINARY: "engram" },
    refusal: /ENGRAM_SOURCE_BINARY must name an absolute engram executable/u },
  { label: "no Git for the run directory", git: false, refusal: /cannot locate Git run directory/u },
  { label: "a refused registration", registration: { status: 409, acceptRegistration: false, errorReason: "fixture refusal" },
    refusal: /registration error:.*renew the claim or name the source root again/su, summary: unregisteredSummary },
  { label: "a claim or fence refusal", registration: { status: 409, acceptRegistration: false,
    errorReason: "named run's claim fence changed; name the source root again" },
    refusal: /claim fence changed/u, summary: unregisteredSummary },
  { label: "an acknowledged registration whose confirmation fails", registration: { queryStatus: 500, errorReason: "fixture query failure" },
    refusal: /registration error:/u, summary: unregisteredSummary },
  { label: "a store in repair through the registration wait",
    registration: { status: 503, acknowledgement: () => inRepair }, wait: { boundMs: 70_000, pause: async () => {} },
    refusal: /no tests started; rerun once the store is ready/u,
    summary: /verification: none; no tests ran: the project's Engram store stayed in repair; once it is ready, /u },
  { label: "a worker that cannot confirm its registration at startup", registration: {}, startup: "foreground",
    summary: /verification: none; no tests ran: the run's worker could not confirm its request or registration before readiness/u },
  { label: "a worker whose request became unreadable before startup", registration: {}, startup: "unreadable request",
    summary: /verification: none; no tests ran: the run's worker could not confirm its request or registration before readiness/u },
  { label: "a detached worker that cannot confirm its registration at startup", registration: {}, startup: "detached",
    refusal: /^detached worker closed before readiness/u,
    summary: /verification: none; no tests ran: the run's worker could not confirm its request or registration before readiness/u },
  // The input kind comes from the run's own creation records, not from a
  // request that failed validation.
  { label: "a worker whose results were replaced by other valid JSON", registration: {}, startup: "replaced results",
    summary: /verification: none; no tests ran: the run's worker could not confirm its request or registration before readiness/u },
  { label: "a refused registration whose failure result cannot be saved",
    registration: { status: 409, acceptRegistration: false, errorReason: "fixture refusal" },
    afterCreated: (directory) => {
      // A directory where the results file was leaves the failure result unwritable.
      const results = join(directory, "results.json");
      rmSync(results);
      mkdirSync(results);
    },
    refusal: /registration error:.*run directory: .*\(its failure result could not be saved: /su },
  pendingRefusal(),
  // The cause, the run directory and the correction outlive a failure result
  // the worker cannot write.
  { label: "a worker whose failure result cannot be saved", registration: {}, startup: "unsavable result",
    refusal: /^worker startup failed before readiness: no tests ran: .*; run directory: .*\(its failure result could not be saved: /su },
  { label: "a worker whose request was replaced by other valid JSON", registration: {}, startup: "replaced request",
    summary: /verification: none; no tests ran: the run's worker could not confirm its request or registration before readiness/u },
  { label: "a plain-input worker whose request now claims committed input", registration: {}, options: { input: "plain" },
    startup: "request claims commit", plain: true },
  { label: "a plain-input launch to an unreachable host", host: "down", options: { input: "plain" },
    refusal: /source owner metadata unavailable/u, plain: true },
];
const startupCreditLine = `verification: none; no tests ran: the run's worker could not confirm its request or registration before readiness; ${correction}`;

// A launch stopped or killed while it registers records no registration
// outcome: its records are left as they stand when registration begins, or,
// killed between its two first records, only its request is. Its summary says
// no tests ran and how to get credit, without claiming the host refused it.
// A plain request left alone has no summary.
const stoppedLaunches = [
  { label: "a committed launch stopped while registering", records: ["request.json", "results.json"] },
  { label: "a committed launch killed between its first two records", records: ["request.json"] },
  { label: "a plain launch killed between its first two records", records: ["request.json"], input: "plain" },
];
test("a committed launch stopped while registering says no tests ran and how to get credit", async (t) => {
  for (const row of stoppedLaunches) {
    const f = fixture(t);
    initializeRepository(f.root);
    let stopped;
    await registrationHost(async (host) => {
      await createRun({ root: f.root, input: row.input ?? "commit",
        stages: [{ name: "focused", command: process.execPath, args: ["-e", "process.exit(0)"] }],
        onRunCreated: (directory) => {
          // The run id is the directory's name, as the request records it.
          stopped = join(f.root, ".git", "stopped", basename(directory));
          mkdirSync(stopped, { recursive: true });
          for (const name of row.records) copyFileSync(join(directory, name), join(stopped, name));
        } }, env(host));
    }, { autoAcknowledge: true });
    if (row.input === "plain") {
      await assert.rejects(summarize(stopped), { code: "ENOENT" }, row.label);
      continue;
    }
    const summary = await summarize(stopped);
    assert.match(summary, /^UNKNOWN /u, `${row.label}: ${summary}`);
    assert.match(summary, /verification: none; no tests ran: its registration was never recorded as complete/u,
      `${row.label}: ${summary}`);
    assert.ok(summary.includes(correction), `${row.label}: ${summary}`);
    assert.doesNotMatch(summary, /refused/u, `${row.label}: ${summary}`);
  }
});

test("every launch refused before its first stage names its cause and the correction for credit", async (t) => {
  const stages = [{ name: "focused", command: process.execPath, args: ["-e", "process.exit(0)"] }];
  for (const row of launchRefusals) {
    const f = fixture(t);
    initializeRepository(f.root);
    let runEnv = { ...hostlessEnv(), ...(row.host ? { TERMAL_SESSION_ID: "fixture-owner", TERMAL_BASE_URL: "http://127.0.0.1:9" } : {}),
      ...row.env };
    if (row.git === false) {
      runEnv.PATH = join(f.root, ".git", "empty-bin");
      mkdirSync(runEnv.PATH);
    }
    const fetchMock = row.host === "down"
      ? t.mock.method(globalThis, "fetch", async () => {
        throw new TypeError("fixture host down", { cause: { code: "ECONNREFUSED" } });
      })
      : row.host ? t.mock.method(globalThis, "fetch", row.host) : undefined;
    let runDir;
    const launch = async (launchEnv) => {
      let refusal;
      try {
        runDir = await createRun({ root: f.root, input: "commit", stages, registrationWait: row.wait,
          onRunCreated: (directory) => { runDir = directory; row.afterCreated?.(directory); }, ...row.options }, launchEnv);
      } catch (error) { refusal = error; }
      return refusal;
    };
    let refusal;
    try {
      if (row.registration) {
        await registrationHost(async (host) => {
          runEnv = env(host);
          refusal = await launch(runEnv);
        }, { autoAcknowledge: true, ...row.registration });
      } else {
        refusal = await launch(runEnv);
      }
    } finally { fetchMock?.mock.restore(); }
    if (row.startup) {
      // A host that never saw the run cannot confirm it at worker startup.
      assert.equal(refusal, undefined, row.label);
      const requestPath = join(runDir, "request.json");
      if (row.startup === "unreadable request") writeFileSync(requestPath, "{");
      if (row.startup === "replaced request") writeFileSync(requestPath, "{}");
      // Valid JSON that fails validation carries no input evidence; the
      // request, which passed its own validation, decides.
      if (row.startup === "replaced results") writeFileSync(join(runDir, "results.json"), "{}");
      if (row.startup === "unsavable result") {
        rmSync(join(runDir, "results.json"));
        mkdirSync(join(runDir, "results.json"));
      }
      if (row.startup === "request claims commit") {
        writeFileSync(requestPath, JSON.stringify({ ...JSON.parse(readFileSync(requestPath, "utf8")), input: "commit" }));
      }
      await registrationHost(async (other) => {
        if (row.startup === "detached") {
          // The refusal ends with the run's own credit line, whatever its log tail holds.
          await assert.rejects(startDetachedRun(runDir, env(other)), (error) => {
            assert.match(error.message, row.refusal, row.label);
            assert.ok(error.message.endsWith(`; ${startupCreditLine}`), `${row.label}: ${error.message}`);
            return true;
          });
        } else if (row.startup === "unsavable result") {
          await assert.rejects(executeRun(runDir, env(other)), (error) => {
            assert.match(error.message, row.refusal, row.label);
            assert.ok(error.message.endsWith(`; ${correction}`), `${row.label}: ${error.message}`);
            return true;
          });
        } else {
          assert.equal((await executeRun(runDir, env(other))).state, "failed", row.label);
        }
      }, { autoAcknowledge: true });
      if (row.plain) assert.ok(!(await summarize(runDir)).includes(correction), row.label);
    } else {
      // Refused before its first stage, the CLI refusal ends with the correction.
      assert.ok(refusal, `${row.label}: refused`);
      assert.match(refusal.message, row.refusal, row.label);
      assert.equal(refusal.message.endsWith(correction), !row.plain, `${row.label}: ${refusal.message}`);
    }
    if (row.summary) {
      const summary = await summarize(runDir);
      assert.match(summary, row.summary, row.label);
      assert.ok(summary.includes(correction), `${row.label}: ${summary}`);
      assert.doesNotMatch(summary, /not registered at start/u, row.label);
    } else if (!row.startup && !row.afterCreated) {
      assert.equal(runDir, undefined, `${row.label}: no run directory`);
    }
  }
});

// A host that refuses the session's source-owner metadata stops the launch
// before it has a run directory, in every input mode. A refusal the host
// explains carries its own reason, escaped and cut on a whole escape at 512
// shown characters with the received length in characters. Only the typed
// refusal for an unconfigured project names its own remedy, since registering
// for credit needs the same metadata; every other refusal, including a busy
// host, keeps the existing correction. A reply without a readable reason
// keeps its status alone.
test("a source-owner refusal names the host's reason and the remedy that fits it", async (t) => {
  const stages = [{ name: "focused", command: process.execPath, args: ["-e", "process.exit(0)"] }];
  const unconfigured = "Engram source owner binaryPath is not configured for this project";
  const long = `bad\u0007reason ${"x".repeat(600)}`;
  const straddle = `${"x".repeat(509)}\u0007z`;
  const astral = "\u{1F600}".repeat(600);
  const replies = [
    { label: "an unconfigured owner with its code", body: { error: unconfigured, code: "engram_source_owner_unconfigured" },
      shown: [unconfigured, "engram_source_owner_unconfigured"], remedy: true },
    { label: "a busy host", status: 429, body: { error: "source owner probe busy" }, shown: ["source owner probe busy"] },
    { label: "a long reason with a control character", body: { error: long },
      shown: ["bad\\u{7}reason", `(${[...long].length} characters)`], hidden: ["\u0007", "x".repeat(512)] },
    { label: "a reason of exactly the bound", body: { error: "y".repeat(512) },
      shown: ["y".repeat(512)], hidden: ["…"] },
    { label: "an escape that would straddle the bound", body: { error: straddle },
      shown: [`${"x".repeat(509)}… (511 characters)`], hidden: ["\\u"] },
    { label: "a reason of astral characters", body: { error: astral },
      shown: [`${"\u{1F600}".repeat(512)}… (600 characters)`], hidden: ["\u{1F600}".repeat(513)] },
    { label: "a reason whose code is not an identifier", body: { error: unconfigured, code: "Not A Code\n" },
      shown: [unconfigured], hidden: ["Not A Code"] },
    { label: "a code with an empty segment", body: { error: unconfigured, code: "engram__unconfigured" },
      shown: [unconfigured], hidden: ["engram__unconfigured"] },
    { label: "a code with a trailing underscore", body: { error: unconfigured, code: "engram_source_owner_unconfigured_" },
      shown: [unconfigured], hidden: ["; code"] },
    { label: "a reason with a backslash", body: { error: "literal \\u{7} text" },
      shown: ["literal \\\\u{7} text"], hidden: ["literal \\u{7} text"] },
    { label: "a reason with a lone surrogate", body: { error: "half \ud800 pair" },
      shown: ["half \\u{d800} pair"], hidden: ["\ud800"] },
    { label: "a reason that is not valid UTF-8",
      raw: Buffer.concat([Buffer.from('{"error":"bad '), Buffer.from([0xff]), Buffer.from(' byte"}')]),
      hidden: ["bad", "�"] },
    { label: "a malformed body", raw: "not json", hidden: ["not json"] },
    { label: "a body without an error", body: { message: "elsewhere" }, hidden: ["elsewhere"] },
    { label: "an oversized body", body: { error: "o".repeat(70_000) }, hidden: ["oooo"] },
  ];
  const modes = [
    { label: "default input", options: { input: undefined, automaticInput: true } },
    { label: "--input commit", options: { input: "commit" } },
    { label: "--input plain", options: { input: "plain" } },
  ];
  const hostEnv = { ...hostlessEnv(), TERMAL_SESSION_ID: "fixture-owner", TERMAL_BASE_URL: "http://127.0.0.1:9" };
  for (const reply of replies) {
    for (const mode of modes) {
      const where = `${reply.label}, ${mode.label}`;
      // The refusal comes before the launcher reads the repository, so the
      // root needs none; nothing may be created in it.
      const f = fixture(t);
      const before = readdirSync(f.root, { recursive: true }).sort();
      const status = reply.status ?? 409;
      const fetchMock = t.mock.method(globalThis, "fetch",
        async () => new Response(reply.raw ?? JSON.stringify(reply.body), { status }));
      let created;
      let refusal;
      try {
        await createRun({ root: f.root, stages, onRunCreated: (directory) => { created = directory; }, ...mode.options }, hostEnv);
      } catch (error) { refusal = error; } finally { fetchMock.mock.restore(); }
      assert.ok(refusal, `${where}: refused`);
      assert.equal(created, undefined, `${where}: no run directory`);
      assert.deepEqual(readdirSync(f.root, { recursive: true }).sort(), before, `${where}: nothing created`);
      const message = refusal.message;
      assert.ok(message.startsWith(`source owner metadata refused (${status}`), `${where}: ${message}`);
      assert.ok(message.includes("no tests started"), `${where}: ${message}`);
      for (const text of reply.shown ?? []) assert.ok(message.includes(text), `${where} shows ${text}: ${message}`);
      for (const text of reply.hidden ?? []) assert.ok(!message.includes(text), `${where} hides ${JSON.stringify(text.slice(0, 20))}: ${message}`);
      if (!reply.shown) {
        assert.ok(message.startsWith(`source owner metadata refused (${status}); no tests started`), `${where}: ${message}`);
      }
      if (reply.remedy) {
        for (const part of ["binaryPath", "hostless", "TERMAL_SESSION_ID", "ENGRAM_SOURCE_BINARY", "evidence only", "--notify"]) {
          assert.ok(message.includes(part), `${where} names ${part}: ${message}`);
        }
        assert.ok(!message.includes("register at start"), `${where}: ${message}`);
        continue;
      }
      // Every other refusal keeps the existing correction: registering for
      // credit in default and commit modes, none for a plain run.
      assert.ok(!message.includes("hostless"), `${where}: ${message}`);
      assert.equal(message.endsWith(correction), mode.options.input !== "plain", `${where}: ${message}`);
    }
  }
});
