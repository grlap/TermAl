import assert from "node:assert/strict";
import { readFileSync, realpathSync } from "node:fs";
import { createHash } from "node:crypto";
import test from "node:test";
import { captureCommitInputStatus, sourceIdsEqual, encodeRunRecord, encodeTerminalRunResult,
  RUN_RECORD_JSON_BYTES } from "./test-launcher-input.mjs";
import { sourceOwnerMetadata } from "./test-run-registration.mjs";

const root = realpathSync.native(process.cwd());
const commit = "a".repeat(40), tree = "b".repeat(40);
const change = (path = "local.txt") => ({ path, status: "??", origin: null,
  blob: "c".repeat(40), mode: "100644" });
const clean = () => ({ commit, tree, submodules: [], changes: [] });
const dirty = () => ({ ...clean(), changes: [change()], submodules: [
  { path: "child/nested", commit: "d".repeat(40), changes: [change("child.txt")] },
] });
const envelope = (sourceId) => ({ version: 1, status: sourceId.changes.length ? "not_clean" : "clean",
  root, source: `git-commit:${sourceId.commit}`, tree: sourceId.tree,
  entries: sourceId.changes.map(({ path, status }) => ({ path, status })), refusal: null, source_id: sourceId });
const reply = (value, status = 0) => ({ status, stdout: Buffer.from(JSON.stringify(value)), stderr: Buffer.alloc(0) });

test("ordinary records admit exact UTF-8 byte fit and refuse one byte over, including the newline", () => {
  const value = { text: "é😀" };
  const overhead = Buffer.byteLength(`${JSON.stringify(value)}\n`);
  value.text += "x".repeat(RUN_RECORD_JSON_BYTES - overhead);
  const encoded = encodeRunRecord(value, "request.json");
  assert.equal(encoded.length, RUN_RECORD_JSON_BYTES);
  assert.equal(encoded.at(-1), 10);
  assert.deepEqual(JSON.parse(encoded), value);
  value.text += "x";
  assert.throws(() => encodeRunRecord(value, "request.json"),
    (error) => error.code === "RUN_RECORD_OUTPUT_LIMIT" && error.artifact === "request.json");
});

test("two complete observations fit at the byte edge without inheriting a one-copy allowance", () => {
  const source = { ...clean(), changes: [change("é😀")] };
  const request = { sourceId: source };
  const result = { sourceIdBefore: source, sourceIdAfter: source, padding: "" };
  source.changes[0].path += "x".repeat(Math.floor((RUN_RECORD_JSON_BYTES
    - Buffer.byteLength(`${JSON.stringify(result)}\n`)) / 2));
  const remaining = RUN_RECORD_JSON_BYTES - Buffer.byteLength(`${JSON.stringify(result)}\n`);
  result.padding = "x".repeat(remaining);
  assert.equal(encodeRunRecord(result, "results.json").length, RUN_RECORD_JSON_BYTES);
  assert.ok(encodeRunRecord(request, "request.json").length < RUN_RECORD_JSON_BYTES);
  result.padding += "x";
  assert.throws(() => encodeRunRecord(result, "results.json"), /output_limit/u);
});

test("post-boundary source overflow preserves original stage facts and a readable unavailable terminal", () => {
  for (const code of [0, 7]) {
    const before = clean();
    const after = { ...clean(), changes: [change("é".repeat(300_000))] };
    const stages = [{ name: "focused", state: code === 0 ? "passed" : "failed", code,
      tests: { passed: 5, failed: code === 0 ? 0 : 1 }, log: "original.log" }];
    const result = { state: code === 0 ? "passed" : "failed", exitCode: code,
      ended: "2026-11-07T00:00:00Z", stages, sourceIdBefore: before, sourceIdAfter: after,
      // Known metadata already consumes part of this record's existing cap.
      metadata: "x".repeat(500_000),
      input: { kind: "commit", commit, cleanBefore: true, eligible: true,
        outcome: "clean", after: { commit, clean: true, paths: [] } } };
    const originalStages = structuredClone(stages);
    const bytes = encodeTerminalRunResult(result);
    assert.ok(bytes.length <= RUN_RECORD_JSON_BYTES);
    const persisted = JSON.parse(bytes);
    assert.deepEqual(persisted.stages, originalStages);
    assert.equal(persisted.exitCode, code || 1);
    assert.equal(persisted.state, "failed");
    assert.deepEqual(persisted.sourceIdBefore, before);
    assert.equal(persisted.sourceIdAfter, null);
    assert.equal(persisted.sourceInputReason, "input-unverified");
    assert.match(persisted.sourceUnavailableReason, /results.json: output_limit/u);
    assert.equal(persisted.input.eligible, false);
    assert.equal(persisted.input.after, undefined);
    assert.equal(persisted.input.outcome, undefined);
  }
});

test("terminal overflow removes the entire opening observation only when it also cannot fit", () => {
  const source = { ...clean(), changes: [change("x".repeat(600_000))] };
  const result = { state: "passed", exitCode: 0, ended: "2026-11-07T00:00:00Z",
    sourceIdBefore: source, sourceIdAfter: source, metadata: "x".repeat(500_000),
    stages: [{ name: "focused", state: "passed", code: 0 }] };
  const persisted = JSON.parse(encodeTerminalRunResult(result));
  assert.equal(persisted.sourceIdBefore, null);
  assert.equal(persisted.sourceIdAfter, null);
  assert.equal(persisted.stages[0].code, 0);
  assert.equal(persisted.state, "failed");
});

test("complete clean and dirty owner values compare without a clean policy", () => {
  for (const source of [clean(), dirty()]) {
    const reordered = { changes: structuredClone(source.changes), submodules: structuredClone(source.submodules),
      tree: source.tree, commit: source.commit };
    assert.equal(sourceIdsEqual(source, reordered), true);
    const captured = captureCommitInputStatus(root, { ENGRAM_SOURCE_BINARY: process.execPath }, {
      run: (_binary, args) => {
        assert.deepEqual(args, ["source", "--root", root, "--json"]);
        return reply(envelope(source));
      },
    });
    assert.deepEqual(captured.sourceId, source);
  }
});

test("root, child, mode and newly untracked changes cannot borrow equal commits", () => {
  const before = dirty();
  const mutations = [
    (value) => { value.commit = tree; },
    (value) => { value.tree = commit; },
    (value) => { value.changes[0].blob = commit; },
    (value) => { value.changes[0].mode = "100755"; },
    (value) => { value.changes.push(change("new-untracked.txt")); },
    (value) => { value.submodules[0].commit = commit; },
    (value) => { value.submodules[0].changes[0].blob = tree; },
    (value) => { value.submodules[0].changes[0].mode = "120000"; },
  ];
  for (const mutate of mutations) {
    const after = structuredClone(before);
    mutate(after);
    assert.equal(sourceIdsEqual(before, after), false);
  }
});

test("missing, malformed and oversized values are never equality, including both null", () => {
  const missingNullable = dirty();
  delete missingNullable.changes[0].origin;
  const extra = { ...clean(), ignoredImage: "not an owner field" };
  const oversized = { ...clean(), changes: [change("x".repeat(1024 * 1024))] };
  for (const source of [null, undefined, {}, missingNullable, extra, oversized]) {
    assert.equal(sourceIdsEqual(source, source), false);
  }
  const duplicatePath = { ...clean(), changes: [change("same.txt"), {
    path: "same.txt", status: "D ", origin: null, blob: null, mode: null,
  }] };
  assert.equal(sourceIdsEqual(duplicatePath, structuredClone(duplicatePath)), true);
  const reversed = structuredClone(duplicatePath);
  reversed.changes.reverse();
  assert.equal(sourceIdsEqual(reversed, reversed), false, "producer order is validated, not repaired");
});

test("unsupported root and child status characters cannot supply an admitted identity", () => {
  for (const child of [false, true]) {
    for (const status of ["! ", " !", "!!", "?!", "XX"]) {
      const source = dirty();
      (child ? source.submodules[0].changes[0] : source.changes[0]).status = status;
      assert.equal(sourceIdsEqual(source, source), false, `${child}/${status}`);
      assert.throws(() => captureCommitInputStatus(root, { ENGRAM_SOURCE_BINARY: process.execPath },
        { run: () => reply(envelope(source)) }), `${child}/${status}`);
    }
  }
});

test("an unavailable owner or a broken envelope never supplies a partial identity", () => {
  for (const code of ["input_unavailable", "deadline", "output_limit"]) {
    const refused = { version: 1, status: "unavailable", root, source: null, tree: null,
      entries: [], source_id: null, refusal: { code, message: "fixture unavailable" } };
    assert.throws(() => captureCommitInputStatus(root, { ENGRAM_SOURCE_BINARY: process.execPath },
      { run: () => reply(refused, 1) }));
  }
  const incoherent = envelope(clean());
  incoherent.source_id.commit = tree;
  assert.throws(() => captureCommitInputStatus(root, { ENGRAM_SOURCE_BINARY: process.execPath },
    { run: () => reply(incoherent) }));
});

test("native metadata refusal never falls back to hostless binary selection", async (t) => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  const calls = [];
  globalThis.fetch = async (url) => {
    calls.push(String(url));
    return new Response("not available", { status: 404 });
  };
  await assert.rejects(sourceOwnerMetadata({ TERMAL_BASE_URL: "http://127.0.0.1:12345",
    TERMAL_SESSION_ID: "session-fixture", TERMAL_CLI: process.execPath,
    ENGRAM_SOURCE_BINARY: process.execPath }));
  assert.deepEqual(calls, ["http://127.0.0.1:12345/api/sessions/session-fixture/source-owner"]);
});

test("native metadata transport failure is named and redacted without retry or fallback", async (t) => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  let calls = 0;
  globalThis.fetch = async () => {
    calls += 1;
    throw new Error("private transport URL https://private.invalid/token-secret", {
      cause: Object.assign(new Error("private cause"), { code: "ECONNREFUSED" }),
    });
  };
  await assert.rejects(sourceOwnerMetadata({ TERMAL_BASE_URL: "http://127.0.0.1:12345",
    TERMAL_SESSION_ID: "session-fixture", ENGRAM_SOURCE_BINARY: "relative-unused-fallback" }), (error) => {
    assert.equal(error.message,
      "source owner metadata unavailable (transport_failure; ECONNREFUSED); no tests started");
    assert.doesNotMatch(error.message, /private|token-secret|https:/u);
    return true;
  });
  assert.equal(calls, 1, "failed metadata cannot retry or use a hostless executable");
});

test("the native metadata pin selects the exact executable and refuses changed bytes before capture", async (t) => {
  const binaryPath = realpathSync.native(process.execPath);
  const pin = { binaryPath, sha256: createHash("sha256").update(readFileSync(binaryPath)).digest("hex"),
    binaryVersion: "engram fixture version", sourceVersion: 1 };
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  globalThis.fetch = async () => new Response(JSON.stringify(pin), { status: 200 });
  const env = { TERMAL_BASE_URL: "http://127.0.0.1:12345", TERMAL_SESSION_ID: "session-fixture",
    ENGRAM_SOURCE_BINARY: "relative-wrong-fallback", CARGO_HOME: "also-not-absolute" };
  const admitted = await sourceOwnerMetadata(env);
  let calls = 0;
  const run = (binary) => { calls += 1; assert.equal(binary, binaryPath); return reply(envelope(clean())); };
  assert.deepEqual(captureCommitInputStatus(root, env, { pin: admitted, run }).sourceId, clean());
  assert.equal(calls, 1);
  assert.throws(() => captureCommitInputStatus(root, env, {
    pin: { ...admitted, sha256: "0".repeat(64) }, run,
  }), /no longer matches its pin/u);
  assert.equal(calls, 1, "a pin failure cannot start an owner or try another executable");
});
