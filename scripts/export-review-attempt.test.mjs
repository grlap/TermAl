import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { exportReviewAttempt } from "./export-review-attempt.mjs";
import { removeFixtureOnPass } from "./test-fixture-cleanup.mjs";

const repository = fileURLToPath(new URL("../", import.meta.url));
async function fixture(t) {
  await fs.mkdir(path.join(repository, ".tmp"), { recursive: true });
  const root = await fs.mkdtemp(path.join(repository, ".tmp", "review-export-"));
  assert.equal(path.dirname(root), path.join(repository, ".tmp"));
  removeFixtureOnPass(t, root);
  return root;
}
function host(attempt, output, corrupt = (p) => p, instance = "host-1", resultId = `review-result-${attempt}`, reviewAttemptState) {
  let revision = 1;
  const packet = { submissionAttempt: attempt, resultId,
    reviewAttemptProvenance: { submission: "validated", rawOutputAvailable: output !== null },
    result: { delegationId: "delegation-1", childSessionId: "child-1", summary: `attempt ${attempt}` } };
  return async (url) => {
    assert.equal(url.searchParams.get("submissionAttempt"), String(attempt));
    if (url.searchParams.has("resultId")) assert.equal(url.searchParams.get("resultId"), resultId);
    const observation = { revision: revision++, serverInstanceId: instance,
      ...(reviewAttemptState === undefined ? {} : { reviewAttemptState }) };
    if (!url.pathname.endsWith("/output")) return Response.json({ ...packet, ...observation });
    assert.notEqual(output, null, "absent raw output must not be requested");
    assert.equal(url.searchParams.get("resultId"), resultId);
    const offset = Number(url.searchParams.get("offsetBytes"));
    const bytes = Buffer.from(output);
    let cut = offset === 0 && bytes.length > 4 ? 4 : bytes.length;
    while (cut < bytes.length && (bytes[cut] & 0xc0) === 0x80) cut--;
    const page = { ...observation, delegationId: "delegation-1", childSessionId: "child-1", submissionAttempt: attempt, resultId,
      output: bytes.subarray(offset, cut).toString("utf8"), offsetBytes: offset, totalBytes: bytes.length,
      complete: cut === bytes.length, summaryFallback: false, ...(cut < bytes.length ? { nextOffsetBytes: cut } : {}) };
    return Response.json(corrupt(page));
  };
}
const options = (outputRoot, attempt, fetcher) => ({ baseUrl: "http://fixture.invalid", parentSessionId: "parent-1",
  delegationId: "delegation-1", submissionAttempt: attempt, outputRoot, repositoryRoot: repository, fetcher });

test("live attempt observations never rewrite or invalidate the original immutable export", async (t) => {
  const root = await fixture(t);
  const observedHost = (state) => host(1, "original prose", p => p, "host-1", "review-result-1", state);
  const first = await exportReviewAttempt(options(root, 1, observedHost({ currentSubmissionAttempt: 1, disposition: "published" })));
  const names = await fs.readdir(first.directory);
  const saved = await Promise.all(names.map(n => fs.readFile(path.join(first.directory, n))));
  for (const state of [
    { currentSubmissionAttempt: 2, disposition: "superseded" },
    { currentSubmissionAttempt: null, disposition: "published_current_unavailable" },
  ]) {
    const repeated = await exportReviewAttempt(options(root, 1, observedHost(state)));
    assert.equal(repeated.unchanged, true);
    assert.deepEqual(repeated.manifest, first.manifest);
    for (const [i, name] of names.entries()) assert.deepEqual(await fs.readFile(path.join(first.directory, name)), saved[i]);
  }
  await assert.rejects(exportReviewAttempt(options(root, 1, host(1, "changed original prose"))), /different bytes/);
});

test("another eligible reader reuses the original export without changing its provenance", async (t) => {
  const root = await fixture(t);
  const first = await exportReviewAttempt(options(root, 1, host(1, "original shared result")));
  assert.equal(first.manifest.requesterSessionId, "parent-1");
  const names = await fs.readdir(first.directory);
  const saved = await Promise.all(names.map(name => fs.readFile(path.join(first.directory, name))));
  for (const reader of ["eligible-root-2", "returning-parent"]) {
    const repeated = await exportReviewAttempt({ ...options(root, 1, host(1, "original shared result")), parentSessionId: reader });
    assert.equal(repeated.unchanged, true);
    assert.deepEqual(repeated.manifest, first.manifest);
    for (const [i, name] of names.entries()) assert.deepEqual(await fs.readFile(path.join(first.directory, name)), saved[i]);
  }
});

test("two attempt exports retain all earlier packet, page and manifest bytes", async (t) => {
  const root = await fixture(t);
  const first = await exportReviewAttempt(options(root, 1, host(1, "old prose and findings")));
  const names = await fs.readdir(first.directory);
  const saved = await Promise.all(names.map((n) => fs.readFile(path.join(first.directory, n))));
  const second = await exportReviewAttempt(options(root, 2, host(2, "new prose")));
  assert.notEqual(first.directory, second.directory);
  for (const [i, n] of names.entries()) assert.deepEqual(await fs.readFile(path.join(first.directory, n)), saved[i]);
  const repeated = await exportReviewAttempt(options(root, 1, host(1, "old prose and findings", p => p, "restarted-host")));
  assert.equal(repeated.unchanged, true);
  assert.deepEqual(repeated.manifest, first.manifest);
  for (const [i, n] of names.entries()) assert.deepEqual(await fs.readFile(path.join(first.directory, n)), saved[i]);
  await assert.rejects(exportReviewAttempt(options(root, 1, host(1, "different prose"))), /different bytes/);
  for (const [i, n] of names.entries()) assert.deepEqual(await fs.readFile(path.join(first.directory, n)), saved[i]);
  assert.equal(await fs.readFile(path.join(first.directory, "full-output.txt"), "utf8"), "old prose and findings");
});

test("mixed attempt, fallback, and missing-page responses never publish an export", async (t) => {
  const root = await fixture(t);
  for (const corrupt of [p => ({ ...p, submissionAttempt: 2 }), p => ({ ...p, resultId: "other" }),
    p => ({ ...p, summaryFallback: true }), p => ({ ...p, nextOffsetBytes: 1 }), p => ({ ...p, totalBytes: 1 })]) {
    await assert.rejects(exportReviewAttempt(options(root, 1, host(1, "two pages", corrupt))), /invalid|mixed/);
    assert.deepEqual(await fs.readdir(root), []);
  }
});

test("UTF-8 output is exported at byte offsets without appended or missing characters", async (t) => {
  const root = await fixture(t);
  const output = "abcżółć";
  const result = await exportReviewAttempt(options(root, 1, host(1, output)));
  const bytes = await fs.readFile(path.join(result.directory, "full-output.txt"));
  assert.deepEqual(bytes, Buffer.from(output));
  assert.equal(result.manifest.output.bytes, Buffer.byteLength(output));
  assert.equal(result.manifest.pages[0].offsetBytes, 0);
  assert.equal(result.manifest.pages[1].offsetBytes, 3, "requested boundary inside ż must retreat to UTF-8 boundary");
});

test("missing raw output retains a packet-only export with an explicit coverage gap", async (t) => {
  const root = await fixture(t);
  const result = await exportReviewAttempt(options(root, 1, host(1, null)));
  assert.deepEqual(result.manifest.output, { available: false, complete: false });
  assert.deepEqual(result.manifest.pages, []);
  assert.deepEqual((await fs.readdir(result.directory)).sort(), ["manifest.json", "packet.json"]);
  const packet = JSON.parse(await fs.readFile(path.join(result.directory, "packet.json")));
  assert.equal(packet.reviewAttemptProvenance.rawOutputAvailable, false);
  assert.equal((await exportReviewAttempt(options(root, 1, host(1, null, p => p, "restarted")))).unchanged, true);
});

test("a tampered stored packet is refused even when only volatile fields were edited", async (t) => {
  const root = await fixture(t);
  const result = await exportReviewAttempt(options(root, 1, host(1, "unchanged prose")));
  const file = path.join(result.directory, "packet.json");
  const packet = JSON.parse(await fs.readFile(file));
  await fs.writeFile(file, JSON.stringify({ ...packet, revision: 999 }));
  await assert.rejects(exportReviewAttempt(options(root, 1, host(1, "unchanged prose"))), /different bytes/);
});

test("a repaired cut of one attempt has a distinct export and an older id remains pinnable", async (t) => {
  const root = await fixture(t);
  const first = await exportReviewAttempt(options(root, 1, host(1, "first cut")));
  const saved = await fs.readFile(path.join(first.directory, "manifest.json"));
  const repaired = await exportReviewAttempt(options(root, 1, host(1, "repaired cut", p => p, "host-2", "review-result-repaired")));
  assert.notEqual(repaired.directory, first.directory);
  assert.equal(path.dirname(repaired.directory), path.dirname(first.directory));
  const pinned = await exportReviewAttempt({ ...options(root, 1, host(1, "first cut")), resultId: first.manifest.resultId });
  assert.equal(pinned.unchanged, true);
  assert.deepEqual(await fs.readFile(path.join(first.directory, "manifest.json")), saved);
});

test("unsafe identifiers and repository-external destinations are refused", async (t) => {
  const root = await fixture(t);
  await assert.rejects(exportReviewAttempt({ ...options(root, 1, host(1, "prose")), delegationId: "../other" }), /explicit/);
  await assert.rejects(exportReviewAttempt({ ...options(root, 1, host(1, "prose")), outputRoot: path.dirname(repository) }), /inside the repository/);
  assert.deepEqual(await fs.readdir(root), []);
});
