import test from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { PassThrough, Writable } from "node:stream";
import { createHash } from "node:crypto";
import { readFile, realpath, mkdir, mkdtemp } from "node:fs/promises";
import { resolve, isAbsolute } from "node:path";
import { probeAgy } from "./agy-acp-contract.mjs";

// Deterministic controller coverage spends no provider quota.
test("ACP contract probe sends one prompt, rejects permission, cancels and reaps", async () => {
  const requests = [];
  let killed = false;
  const child = new EventEmitter();
  child.stdout = new PassThrough();
  child.stderr = new PassThrough();
  const reply = frame => queueMicrotask(() => child.stdout.write(JSON.stringify(frame) + "\n"));
  child.stdin = new Writable({ write(chunk, _encoding, done) {
    const frame = JSON.parse(chunk.toString());
    requests.push(frame);
    if (frame.method === "initialize") reply({ id: frame.id, result: {
      protocolVersion: 1, authMethods: [{ id: "oauth-personal" }],
      agentCapabilities: { loadSession: true, sessionCapabilities: { resume: {} } },
    } });
    if (frame.method === "session/new") reply({ id: frame.id, result: { sessionId: "fixture" } });
    if (frame.method === "session/prompt") reply({ id: "permission", method: "session/request_permission",
      params: { sessionId: "fixture", options: [{ kind: "reject_once", optionId: "deny" }] } });
    if (frame.method === "session/cancel") reply({ id: 3, result: { stopReason: "cancelled" } });
    done();
  } });
  child.kill = () => { killed = true; queueMicrotask(() => child.emit("close", 0)); };
  const report = await probeAgy({ binary: "fixture", cwd: process.cwd(), launch: (_binary, args) => {
    assert.deepEqual(args, ["acp"]);
    return child;
  } });
  assert.equal(requests.filter(frame => frame.method === "session/prompt").length, 1);
  assert.deepEqual(requests.find(frame => frame.id === "permission").result,
    { outcome: { outcome: "selected", optionId: "deny" } });
  assert.equal(report.permissionResponse, "reject_once");
  assert.equal(report.cancelSent, true);
  assert.equal(report.loadSession, true);
  assert.equal(report.resume, true);
  assert.equal(killed, true);
});


test("ACP contract probe reports blocked auth without a prompt or login", async () => {
  const methods = [];
  const child = new EventEmitter();
  child.stdout = new PassThrough();
  child.stderr = new PassThrough();
  child.stdin = new Writable({ write(chunk, _encoding, done) {
    const frame = JSON.parse(chunk.toString());
    methods.push(frame.method);
    queueMicrotask(() => child.stdout.write(JSON.stringify(frame.method === "initialize"
      ? { id: frame.id, result: { protocolVersion: 1, authMethods: [{ id: "oauth-personal" }] } }
      : { id: frame.id, error: { code: -32000, message: "Authentication required" } }) + "\n"));
    done();
  } });
  let killed = false;
  child.kill = () => { killed = true; queueMicrotask(() => child.emit("close", 0)); };
  await assert.rejects(probeAgy({ binary: "fixture", cwd: process.cwd(), launch: () => child }),
    /blocked-on-auth at session\/new/);
  assert.deepEqual(methods, ["initialize", "session/new"]);
  assert.equal(killed, true);
});

test("real Antigravity ACP contract (one prompt; operator opt-in)", {
  skip: process.env.TERMAL_AGY_CONTRACT_SMOKE !== "1",
  timeout: 105_000,
}, async t => {
  assert.ok(process.env.TERMAL_AGY_SMOKE_BINARY, "set an absolute native agy binary path");
  assert.ok(isAbsolute(process.env.TERMAL_AGY_SMOKE_BINARY), "binary path must be absolute");
  const binary = await realpath(process.env.TERMAL_AGY_SMOKE_BINARY);
  const sha256 = createHash("sha256").update(await readFile(binary)).digest("hex");
  // Empty owned workspace, isolated from repository instructions and files.
  // The CLI still consumes its own existing cached sign-in; no HOME override.
  await mkdir(resolve(".tmp"), { recursive: true });
  const cwd = await mkdtemp(resolve(".tmp", "agy-acp-contract-"));
  try {
    const report = await probeAgy({ binary, cwd });
    t.diagnostic(JSON.stringify({ binary, sha256, cwd, ...report }));
    assert.equal(report.promptCount, 1);
    assert.ok(report.permissionResponse);
    assert.equal(report.cancelSent, true);
  } catch (error) {
    t.diagnostic(JSON.stringify({ binary, sha256, cwd, ...error.report,
      status: String(error.message).startsWith("blocked-on-auth") ? "blocked-on-auth" : "failed" }));
    throw error;
  }
});
