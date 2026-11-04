import assert from "node:assert/strict";
import fs from "node:fs/promises";
import { lstatSync, mkdirSync, mkdtempSync, realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { collectReport, contains, parseOptions, parseWorktrees, readNamedRoots, readRetentions, shapeReport, validateRetentions } from "./cleanup-report.mjs";
import { isolatedGitEnvironment } from "./review-freeze-fingerprint.mjs";
import { removeFixture } from "./test-fixture-cleanup.mjs";

const project = realpathSync(fileURLToPath(new URL("..", import.meta.url)));
function fixture(t) {
  const parent = path.join(project, ".tmp");
  mkdirSync(parent, { recursive: true });
  assert(!lstatSync(parent).isSymbolicLink());
  const root = mkdtempSync(path.join(parent, "cleanup-report-test-"));
  t.after(() => {
    if (!t.passed) {
      const verdict = t.error?.failureType === "testTimeoutFailure" ? "hung" : "failed";
      process.stderr.write(`${verdict}: ${t.name}; fixture kept at ${root}\n`);
      return;
    }
    const resolved = realpathSync(root);
    assert(contains(project, resolved) && contains(parent, resolved));
    assert(!lstatSync(root).isSymbolicLink());
    removeFixture(resolved);
  });
  return root;
}
const unknownRoots = { state: "unknown", reason: "not supplied", roots: [] };
const tree = worktree => ({ worktree, HEAD: "a".repeat(40) });

test("NUL-delimited worktrees preserve spaces, quotes and newlines", () => {
  const location = path.join(project, 'a "quoted"\nworktree');
  const parsed = parseWorktrees(`worktree ${location}\0HEAD ${"a".repeat(40)}\0detached\0\0`);
  assert.equal(parsed[0].worktree, location);
  assert.equal(parsed[0].detached, true);
  assert.throws(() => parseWorktrees("worktree missing-head\0\0"), /Incomplete/);
});

test("path containment respects component boundaries and platform case rules", () => {
  const root = path.join(project, "sample");
  assert(contains(root, path.join(root, "child")));
  assert(contains(root, root));
  assert(!contains(root, root + "-other"));
  assert(!contains(root, path.join(root, "..", "sibling")));
  assert(!contains(root, "\\\\?\\UNC\\server\\share"));
  if (process.platform === "win32") {
    assert(contains(root.toUpperCase(), root.toLowerCase()));
    assert(contains("\\\\?\\" + root, root));
  }
});

test("report distinguishes current roots, reported retention and unknown use without walking contents", async t => {
  const root = fixture(t);
  for (const name of [".tmp/active/child", ".tmp/active-other", ".logs", ".evidence/kept"]) await fs.mkdir(path.join(root, name), { recursive: true });
  await fs.writeFile(path.join(root, ".logs/plain.log"), "abc");
  await fs.writeFile(path.join(root, ".evidence/kept/decision.json"), "keep bytes");
  const readdirCalls = [];
  const statCalls = [];
  const filesystem = {
    async lstat(location) { statCalls.push(location); return fs.lstat(location); },
    async readdir(location) {
      readdirCalls.push(location);
      assert([".tmp", ".logs", ".evidence"].some(name => location === path.join(root, name)));
      return fs.readdir(location);
    },
  };
  const retentions = validateRetentions({ observedAt: "2026-01-01T00:00:00Z", entries: [{ path: ".evidence/kept/decision.json", owner: "investigator", reason: "open investigation", until: "investigation closes" }] }, root);
  const report = await collectReport({ repository: root, worktrees: [tree(root), tree(root + "-outside")], filesystem, retentions,
    namedRoots: { state: "known", roots: [{ root: path.join(root, ".tmp/active/child"), workRef: "example" }, { root: root + "-outside", workRef: "external" }] } });
  const get = suffix => report.entries.find(r => r.path === path.join(root, suffix));
  assert.equal(get(".tmp/active").classification, "retain");
  assert.equal(get(".tmp/active-other").classification, "review");
  assert.equal(get(".evidence/kept").owner, "unknown");
  assert.equal(get(".evidence/kept").retentions[0].owner, "investigator");
  assert.equal(get(".evidence/kept").classification, "retain");
  assert.equal(get(".logs/plain.log").logicalBytes, 3);
  assert.equal(get(".tmp/active-other").logicalBytes, null);
  assert.equal(get(".tmp/active-other").openItemReferences, "unknown");
  assert.equal(get(".tmp/active-other").processUse, "unknown");
  assert.equal(report.entries.find(r => r.path === root + "-outside").type, "outside-repository");
  assert.equal(report.entries.find(r => r.path === root + "-outside").classification, "retain");
  assert.equal(report.entries.find(r => r.path === root).owner, "unknown");
  assert(!statCalls.includes(root + "-outside"));
  assert.equal(readdirCalls.length, 3);
  assert.equal(await fs.readFile(path.join(root, ".evidence/kept/decision.json"), "utf8"), "keep bytes");
  assert.deepEqual((await fs.readdir(root)).sort(), [".evidence", ".logs", ".tmp"]);
});

test("linked groups and linked entries are reported without traversing their targets", async t => {
  const root = fixture(t);
  const target = path.join(root, "destination");
  await fs.mkdir(target);
  await fs.writeFile(path.join(target, "secret.log"), "untouched");
  await fs.symlink(target, path.join(root, ".logs"), process.platform === "win32" ? "junction" : "dir");
  await fs.mkdir(path.join(root, ".tmp"));
  await fs.symlink(target, path.join(root, ".tmp/linked"), process.platform === "win32" ? "junction" : "dir");
  const filesystem = {
    lstat: fs.lstat,
    async readdir(location) { assert.equal(location, path.join(root, ".tmp")); return fs.readdir(location); },
  };
  const report = await collectReport({ repository: root, worktrees: [], namedRoots: unknownRoots, filesystem });
  assert.equal(report.entries.find(r => r.path === path.join(root, ".logs")).type, "link");
  assert.equal(report.entries.find(r => r.path === path.join(root, ".tmp/linked")).type, "link");
  assert.equal(report.warnings.length, 1);
  assert(!report.entries.some(r => r.path.endsWith("secret.log")));
  assert.equal(await fs.readFile(path.join(target, "secret.log"), "utf8"), "untouched");
});

test("missing or malformed host data stays unknown rather than proving no named roots", async () => {
  const absent = await readNamedRoots(undefined, () => { throw new Error("must not fetch"); });
  assert.equal(absent.state, "unknown");
  for (const payload of [{}, [{ root: "relative/path", shortRef: "example" }], [{ root: project }]]) {
    const result = await readNamedRoots("http://localhost:8787", async () => ({ ok: true, json: async () => payload }));
    assert.equal(result.state, "unknown");
    assert.deepEqual(result.roots, []);
  }
  const unavailable = await readNamedRoots("http://localhost:8787", async () => { throw new Error("offline"); });
  assert.equal(unavailable.state, "unknown");
  const valid = await readNamedRoots("http://localhost:8787/base", async (url, options) => {
    assert.equal(url.pathname, "/api/engram/named-roots");
    assert(options.signal);
    return { ok: true, json: async () => [{ root: project, shortRef: "example", workId: "opaque-id" }] };
  });
  assert.equal(valid.state, "known");
  assert.equal(valid.roots[0].workRef, "example");
});

test("unreadable groups remain visible as warnings", async t => {
  const root = fixture(t);
  const filesystem = { async lstat() { throw Object.assign(new Error("denied"), { code: "EACCES" }); } };
  const report = await collectReport({ repository: root, worktrees: [tree(root)], namedRoots: unknownRoots, filesystem });
  assert.equal(report.warnings.length, 3);
  assert.equal(report.entries[0].type, "unreadable");
  assert.equal(report.entries[0].git.dirty, "unknown");
  assert.equal(report.namedRoots.state, "unknown");
  assert.equal(report.entries[0].namedRoots, null);
  assert.equal(report.entries[0].retentions, null);
  assert.equal(report.unmatchedRetentions, null);
  const retained = await collectReport({ repository: root, worktrees: [tree(root), tree(path.join(root, "named"))],
    namedRoots: { state: "known", roots: [{ root: path.join(root, "named"), workRef: "example" }] }, filesystem });
  const row = retained.entries.find(r => r.path === path.join(root, "named"));
  assert.equal(row.classification, "retain");
  assert.match(row.reason, /current named root/);
  assert.match(row.reason, /metadata unavailable/);
});

test("retention input needs an owner and end condition and cannot escape the repository", () => {
  const input = { observedAt: "2026-01-01T00:00:00Z", entries: [{ path: ".tmp/example", owner: "owner", reason: "needed", until: "work ends" }] };
  assert.equal(validateRetentions(input, project).entries.length, 1);
  assert.throws(() => validateRetentions({ ...input, observedAt: "bad" }, project));
  assert.throws(() => validateRetentions({ ...input, entries: [{ ...input.entries[0], until: "" }] }, project));
  assert.throws(() => validateRetentions({ ...input, entries: [{ ...input.entries[0], path: "../outside" }] }, project), /outside/);
  assert.throws(() => validateRetentions({ ...input, entries: [{ ...input.entries[0], path: "\\\\?\\UNC\\server\\share" }] }, project), /outside/);
});

test("parent retentions protect children and unmatched needs remain visible outside a limited view", async t => {
  const root = fixture(t);
  await fs.mkdir(path.join(root, ".evidence"));
  await fs.writeFile(path.join(root, ".evidence/decision.log"), "keep");
  const retentions = validateRetentions({ observedAt: "2026-01-01T00:00:00Z", entries: [
    { path: ".evidence", owner: "owner", reason: "shared evidence", until: "investigation closes" },
    { path: ".tmp/missing", owner: "other owner", reason: "pointer to investigate", until: "pointer resolved" },
  ] }, root);
  const report = await collectReport({ repository: root, worktrees: [tree(root)], namedRoots: unknownRoots, retentions });
  assert.equal(report.entries.find(r => r.path.endsWith("decision.log")).classification, "retain");
  const limited = shapeReport(report, { json: false, limit: 1 });
  assert.equal(report.entries.find(r => r.path.endsWith("decision.log")).owner, "owner");
  assert.equal(limited.unmatchedRetentions.length, 1);
  assert.equal(limited.unmatchedRetentions[0].owner, "other owner");
  assert.equal(report.entries.find(r => r.path === root).owner, "unknown");
});

test("limited view prioritizes review, counts omissions and leaves complete JSON unchanged", () => {
  const report = { counts: { total: 3 }, entries: [
    { path: "a", classification: "retain" }, { path: "c", classification: "review" }, { path: "b", classification: "review" },
  ] };
  const limited = shapeReport(report, { json: false, limit: 1 });
  assert.equal(limited.entries[0].path, "b");
  assert.equal(limited.omitted, 2);
  assert.equal(report.entries.length, 3);
  assert.equal(shapeReport(report, { json: true }).entries.length, 3);
});

test("retention reader rejects stores, links, directories and oversized snapshots", async t => {
  const root = fixture(t);
  await fs.mkdir(path.join(root, "directory.json"));
  const snapshot = path.join(root, "valid.json");
  await fs.writeFile(snapshot, JSON.stringify({ observedAt: "2026-01-01T00:00:00Z", entries: [] }));
  assert.equal((await readRetentions("valid.json", root)).entries.length, 0);
  await assert.rejects(readRetentions("store.db", root), /JSON snapshot/);
  await assert.rejects(readRetentions("directory.json", root), /plain JSON/);
  await fs.symlink(process.platform === "win32" ? path.join(root, "directory.json") : snapshot,
    path.join(root, "linked.json"), process.platform === "win32" ? "junction" : "file");
  await assert.rejects(readRetentions("linked.json", root), /plain JSON/);
  const large = await fs.open(path.join(root, "large.json"), "w");
  try { await large.truncate(8 * 1024 * 1024 + 1); } finally { await large.close(); }
  await assert.rejects(readRetentions("large.json", root), /8 MiB/);
  await assert.rejects(readRetentions("../outside.json", root), /outside/);
});

test("successful CLI uses Git discovery through an alias and imports remain quiet", async t => {
  const base = fixture(t);
  const root = path.join(base, "main");
  await fs.mkdir(root);
  const env = { ...isolatedGitEnvironment(), TERMAL_BASE_URL: "" };
  const init = spawnSync("git", ["init", "--quiet", root], { env, encoding: "utf8", windowsHide: true });
  assert.equal(init.status, 0, init.stderr);
  const commit = spawnSync("git", ["-C", root, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "--allow-empty", "--quiet", "-m", "fixture"], { env, encoding: "utf8", windowsHide: true });
  assert.equal(commit.status, 0, commit.stderr);
  const linkedWorktree = path.join(base, "linked-worktree");
  const linked = spawnSync("git", ["-C", root, "worktree", "add", "--detach", "--quiet", linkedWorktree, "HEAD"], { env, encoding: "utf8", windowsHide: true });
  assert.equal(linked.status, 0, linked.stderr);
  await fs.mkdir(path.join(root, ".evidence"));
  await fs.writeFile(path.join(root, ".evidence/retentions.json"), JSON.stringify({ observedAt: "2026-01-01T00:00:00Z", entries: [] }));
  // Every link target stays inside this fixture, including after a crashed test.
  const privateCopy = path.join(base, "private-checkout");
  await fs.mkdir(path.join(privateCopy, "scripts"), { recursive: true });
  for (const name of ["cleanup-report.mjs", "review-freeze-fingerprint.mjs"]) {
    await fs.copyFile(path.join(project, "scripts", name), path.join(privateCopy, "scripts", name));
  }
  const alias = path.join(base, "checkout-alias");
  await fs.symlink(privateCopy, alias, process.platform === "win32" ? "junction" : "dir");
  const script = path.join(alias, "scripts/cleanup-report.mjs");
  const run = args => spawnSync(process.execPath, [script, ...args], { cwd: linkedWorktree, env, encoding: "utf8", windowsHide: true });
  const all = run(["--json", "--retentions", ".evidence/retentions.json"]);
  assert.equal(all.status, 0, all.stderr);
  const report = JSON.parse(all.stdout);
  assert.equal(realpathSync(report.repository), realpathSync(root));
  assert.equal(report.entries.length, report.counts.total);
  assert.equal(report.namedRoots.state, "unknown");
  const limited = run(["--limit", "1"]);
  assert.equal(limited.status, 0, limited.stderr);
  assert.equal(JSON.parse(limited.stdout).entries.length, 1);
  assert.equal(JSON.parse(limited.stdout).omitted, report.counts.total - 1);
  const imported = spawnSync(process.execPath, ["--input-type=module", "-e", `await import(${JSON.stringify(pathToFileURL(script).href)})`], { cwd: root, env, encoding: "utf8", windowsHide: true });
  assert.equal(imported.status, 0, imported.stderr);
  assert.equal(imported.stdout, "");
});

test("retention snapshot through a linked parent cannot escape its repository", async t => {
  const base = fixture(t), root = path.join(base, "repository"), other = path.join(base, "private-other");
  await fs.mkdir(root);
  await fs.mkdir(other);
  await fs.writeFile(path.join(other, "valid.json"), JSON.stringify({ observedAt: "2026-01-01T00:00:00Z", entries: [] }));
  await fs.symlink(other, path.join(root, "linked-parent"), process.platform === "win32" ? "junction" : "dir");
  await assert.rejects(readRetentions("linked-parent/valid.json", root), /resolves outside/);
});

test("CLI rejects mutation flags before any repository or network operation", () => {
  assert.deepEqual(parseOptions(["--limit", "2", "--json"]), { json: true, limit: 2, retentions: null });
  assert.throws(() => parseOptions(["--limit", "0"]));
  const result = spawnSync(process.execPath, [fileURLToPath(new URL("./cleanup-report.mjs", import.meta.url)), "--delete"], { cwd: project, encoding: "utf8", windowsHide: true });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Usage:/);
  assert.equal(result.stdout, "");
});
