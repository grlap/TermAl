// Exercises registration wire and acknowledgement behavior using private HTTP
// fixtures. No live host, native verification credit or product gate is minted.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { execFileSync, spawn } from "node:child_process";
import { createRun, executeRun, startDetachedRun } from "./test-launcher.mjs";
import { registerTestRun, validateRunRegistration, validateRequestMessageId } from "./test-run-registration.mjs";
import { registrationHost } from "./test-launcher-registration-fixtures.mjs";
import { removeFixtureOnPass } from "./test-fixture-cleanup.mjs";
import { isolatedGitEnvironment } from "./review-freeze-fingerprint.mjs";
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
function env(host) { return { ...process.env, TERMAL_SESSION_ID: "fixture-owner", TERMAL_BASE_URL: host.url, TERMAL_CLI: process.execPath }; }

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
