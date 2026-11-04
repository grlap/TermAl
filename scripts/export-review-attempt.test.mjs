import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { exportReviewAttempt } from "./export-review-attempt.mjs";
import { removeFixture, removeFixtureOnPass } from "./test-fixture-cleanup.mjs";

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

// A clock that only moves when the wait sleeps, so no case depends on real time.
function sharingClock() {
  let elapsed = 0;
  const sleeps = [];
  return { sleeps, now: () => elapsed, advance: (milliseconds) => { elapsed += milliseconds; }, sleep: async (milliseconds) => {
    sleeps.push(milliseconds);
    elapsed += milliseconds;
  } };
}
const sharingError = (code) => Object.assign(new Error(`rename ${code}`), { code });
const attemptEntries = (root) => fs.readdir(path.join(root, "delegation-1", "attempt-1"));
// On Windows another process can briefly hold a new file, and failure cleanup
// does not wait for it, so only other platforms can require staging to be gone.
async function assertNothingPublished(root) {
  const entries = await attemptEntries(root);
  assert.ok(!entries.includes("review-result-1"), "nothing was published");
  if (process.platform !== "win32") assert.deepEqual(entries, []);
}

test("a real handle inside staging refuses the rename, and publication waits for its release", {
  skip: process.platform !== "win32" && "a held child refuses a directory rename only on Windows",
}, async (t) => {
  const root = await fixture(t);
  let held;
  const refusals = [];
  const sleeps = [];
  const sleep = async (milliseconds) => {
    sleeps.push(milliseconds);
    if (held) {
      // The first backoff releases this test's handle. A later one means
      // another process (a scanner) briefly holds the new file too; it
      // waits as the real sleep would.
      await held.close();
      held = undefined;
    } else {
      await new Promise((resolve) => setTimeout(resolve, milliseconds));
    }
  };
  const rename = async (staging, destination) => {
    if (refusals.length === 0) held ??= await fs.open(path.join(staging, "packet.json"), "r");
    try { await fs.rename(staging, destination); }
    catch (error) { refusals.push(error.code); throw error; }
  };
  let result;
  try {
    result = await exportReviewAttempt({ ...options(root, 1, host(1, "held once")),
      publish: { rename, sleep } });
  } finally {
    await held?.close();
  }
  assert.equal(refusals[0], "EPERM", "the held handle is what refused the first rename");
  assert.equal(sleeps[0], 10);
  assert.equal(result.unchanged, false);
  assert.equal(result.publishWait.attempts, refusals.length + 1);
  assert.equal(await fs.readFile(path.join(result.directory, "full-output.txt"), "utf8"), "held once");
  assert.deepEqual(await attemptEntries(root), ["review-result-1"], "no staging directory remains");
});

test("a real handle that is never released fails with the first refusal at the budget", {
  skip: process.platform !== "win32" && "a held child refuses a directory rename only on Windows",
}, async (t) => {
  const root = await fixture(t);
  let held;
  const clock = sharingClock();
  const rename = async (staging, destination) => {
    held ??= await fs.open(path.join(staging, "packet.json"), "r");
    await fs.rename(staging, destination);
  };
  try {
    await assert.rejects(exportReviewAttempt({ ...options(root, 1, host(1, "held forever")),
      publish: { rename, now: clock.now, sleep: clock.sleep, budgetMs: 1000 } }),
    (error) => error.code === "EPERM" && error.syscall === "rename");
  } finally {
    await held?.close();
  }
  assert.ok(clock.sleeps.length > 1, "it waited before giving up");
  assert.ok(!(await attemptEntries(root)).includes("review-result-1"), "nothing was published");
});

test("a sharing error on Windows is waited out with a doubling backoff", async (t) => {
  const root = await fixture(t);
  const clock = sharingClock();
  let calls = 0;
  const rename = async () => {
    calls += 1;
    if (calls <= 3) throw sharingError(["EPERM", "EACCES", "EBUSY"][calls - 1]);
  };
  const result = await exportReviewAttempt({ ...options(root, 1, host(1, "waited")),
    publish: { platform: "win32", rename, now: clock.now, sleep: clock.sleep } });
  assert.equal(calls, 4);
  assert.deepEqual(clock.sleeps, [10, 20, 40]);
  assert.equal(result.unchanged, false);
  assert.deepEqual(result.publishWait, { attempts: 4, elapsedMs: 70 });
});

test("the wait reports the first error at the budget and starts no attempt after it", async (t) => {
  const root = await fixture(t);
  const clock = sharingClock();
  const errors = [];
  const rename = async () => { const error = sharingError("EPERM"); errors.push(error); throw error; };
  await assert.rejects(exportReviewAttempt({ ...options(root, 1, host(1, "refused")),
    publish: { platform: "win32", rename, now: clock.now, sleep: clock.sleep, budgetMs: 100 } }),
  (error) => error === errors[0] && error.publishAttempts === 4 && error.publishElapsedMs === 100);
  assert.deepEqual(clock.sleeps, [10, 20, 40, 30]);
  assert.equal(errors.length, 4, "the sleep that reaches the budget is followed by no attempt");
  await assertNothingPublished(root);
});

test("other errors and other platforms fail at once", async (t) => {
  const root = await fixture(t);
  for (const publish of [{ platform: "win32", code: "ENOSPC" }, { platform: "linux", code: "EPERM" }]) {
    const clock = sharingClock();
    let calls = 0;
    const rename = async () => { calls += 1; throw sharingError(publish.code); };
    await assert.rejects(exportReviewAttempt({ ...options(root, 1, host(1, "refused")),
      publish: { platform: publish.platform, rename, now: clock.now, sleep: clock.sleep } }),
    (error) => error.code === publish.code);
    assert.equal(calls, 1);
    assert.deepEqual(clock.sleeps, []);
    await assertNothingPublished(root);
  }
});

test("a refusal because another export published first settles without waiting", async (t) => {
  const root = await fixture(t);
  const clock = sharingClock();
  let calls = 0;
  const rename = async () => {
    calls += 1;
    await exportReviewAttempt(options(root, 1, host(1, "raced")));
    throw sharingError("EPERM");
  };
  const result = await exportReviewAttempt({ ...options(root, 1, host(1, "raced")),
    publish: { platform: "win32", rename, now: clock.now, sleep: clock.sleep } });
  assert.equal(calls, 1);
  assert.deepEqual(clock.sleeps, []);
  assert.equal(result.unchanged, true);
  assert.equal(result.publishWait, undefined);
  const entries = await attemptEntries(root);
  assert.ok(entries.includes("review-result-1"));
  if (process.platform !== "win32") assert.deepEqual(entries, ["review-result-1"]);
});

test("a held staging file cannot fail an export whose identical copy is published", async (t) => {
  const root = await fixture(t);
  const realUnlink = fs.unlink;
  t.after(() => { fs.unlink = realUnlink; });
  const rename = async (staging) => {
    await exportReviewAttempt(options(root, 1, host(1, "held and raced")));
    // The holder keeps these names in use: removing them fails, here only.
    fs.unlink = async (location) => {
      if (location.startsWith(staging)) throw sharingError("EPERM");
      return realUnlink(location);
    };
    throw sharingError("EPERM");
  };
  const result = await exportReviewAttempt({ ...options(root, 1, host(1, "held and raced")),
    publish: { platform: "win32", rename } });
  fs.unlink = realUnlink;
  assert.equal(result.unchanged, true);
  assert.equal(await fs.readFile(path.join(result.directory, "full-output.txt"), "utf8"), "held and raced");
});

test("fixture removal waits out a refusal on Windows and reports the first error at the budget", () => {
  const clock = sharingClock();
  const syncSleep = (milliseconds) => { clock.sleeps.push(milliseconds); clock.advance(milliseconds); };
  let calls = 0;
  removeFixture("fixture", { platform: "win32", now: clock.now, sleep: syncSleep, remove: () => {
    calls += 1;
    if (calls <= 2) throw sharingError(calls === 1 ? "EPERM" : "ENOTEMPTY");
  } });
  assert.equal(calls, 3);
  assert.deepEqual(clock.sleeps, [50, 50]);

  const errors = [];
  assert.throws(() => removeFixture("fixture", { platform: "win32", now: clock.now, sleep: syncSleep, budgetMs: 100,
    remove: () => { const error = sharingError("EPERM"); errors.push(error); throw error; } }),
  (error) => error === errors[0]);
  assert.equal(errors.length, 3, "the attempt at the budget is the last");
});

test("fixture removal fails at once on other errors and other platforms", () => {
  for (const [platform, code] of [["win32", "ENOSPC"], ["linux", "EPERM"]]) {
    let calls = 0;
    assert.throws(() => removeFixture("fixture", { platform,
      sleep: () => assert.fail("no wait"), remove: () => { calls += 1; throw sharingError(code); } }),
    (error) => error.code === code);
    assert.equal(calls, 1);
  }
});

test("unsafe identifiers and repository-external destinations are refused", async (t) => {
  const root = await fixture(t);
  await assert.rejects(exportReviewAttempt({ ...options(root, 1, host(1, "prose")), delegationId: "../other" }), /explicit/);
  await assert.rejects(exportReviewAttempt({ ...options(root, 1, host(1, "prose")), outputRoot: path.dirname(repository) }), /inside the repository/);
  assert.deepEqual(await fs.readdir(root), []);
});
