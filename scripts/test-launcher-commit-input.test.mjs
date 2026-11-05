// Automatic launcher input selection, explicit plain/commit, and post-run
// qualification using the existing committed-input status and fingerprints.
// Disposable Git repositories live under this worktree's .tmp/; stages are
// plain Node commands, so no product or Cargo test runs.
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import {
  copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, utimesSync,
  writeFileSync, renameSync,
} from "node:fs";
import { dirname, isAbsolute, join, relative } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  captureCommitInputStatus, commitInputGitArgs, isolatedGitEnvironment,
} from "./review-freeze-fingerprint.mjs";
import {
  commitInputSummaryLine,
  commitInputOutcome,
  commitInputStatusClean,
  createRun,
  executeRun,
  helperSupportFiles,
  ORDINARY_COMMIT_INPUT_DEFAULT,
  summarize,
} from "./test-launcher.mjs";
import { registrationHost, withHostedRegistrationFixture } from "./test-launcher-registration-fixtures.mjs";
import { removeFixtureOnPass } from "./test-fixture-cleanup.mjs";

import { writeUiBootstrapFixture } from "./test-ui-bootstrap-fixtures.mjs";

const capacity = { statfs: () => ({ bavail: 200n * 1024n ** 3n, bsize: 1n }) };
const sourceRoot = realpathSync.native(fileURLToPath(new URL("../", import.meta.url)));
const json = (path) => JSON.parse(readFileSync(path, "utf8"));
const stage = (name, source = "") => ({ name, command: process.execPath, args: ["-e", source] });
const marker = "stage-executed.marker";
const markingStage = (admin, name = "fixture", extra = "") => stage(
  name,
  `require('node:fs').appendFileSync(${JSON.stringify(join(admin, marker))}, 'x');${extra}`,
);

function fixtureEnv() {
  const env = isolatedGitEnvironment({
    ...process.env,
    // A missed fixture override must fail locally, never reach the live host.
    TERMAL_BASE_URL: "http://127.0.0.1:0",
    TERMAL_SESSION_ID: "commit-input-fixture-owner",
    TERMAL_CLI: process.execPath,
  });
  delete env.NODE_TEST_CONTEXT;
  return env;
}

function gitRepository(t, files = {}, { full = false } = {}) {
  const directory = join(sourceRoot, ".tmp", "launcher-commit-input-fixtures");
  mkdirSync(directory, { recursive: true });
  const canonical = realpathSync.native(directory);
  const fromSource = relative(sourceRoot, canonical);
  assert.ok(fromSource && !isAbsolute(fromSource) && !fromSource.split(/[\\/]/u).includes(".."));
  const root = mkdtempSync(join(canonical, "commit-input-"));
  removeFixtureOnPass(t, root);
  const env = fixtureEnv();
  const git = (args) => execFileSync("git", args, { cwd: root, env, encoding: "utf8", windowsHide: true });
  git(["init", "--quiet", "--template="]);
  for (const [key, value] of [
    ["user.name", "TermAl Test"], ["user.email", "test@example.invalid"],
    ["core.autocrlf", "false"],
    ["core.hooksPath", process.platform === "win32" ? "NUL" : "/dev/null"],
  ]) git(["config", key, value]);
  writeFileSync(join(root, ".gitignore"), "/.tmp/\nignored-*\n");
  const uiFiles = [];
  if (full) {
    const ignored = readFileSync(join(root, ".gitignore"), "utf8");
    Object.assign(env, writeUiBootstrapFixture(root, join(root, ".git")));
    writeFileSync(join(root, ".gitignore"), `${ignored}/ui/node_modules/\n`);
    uiFiles.push("ui/package.json", "ui/package-lock.json");
  }
  writeFileSync(join(root, "tracked.txt"), "baseline\n");
  for (const [path, source] of Object.entries(files)) {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    copyFileSync(source, join(root, path));
  }
  mkdirSync(join(root, ".git", "info"), { recursive: true });
  writeFileSync(join(root, ".git", "info", "exclude"), "excluded.txt\n");
  git(["add", "--", ".gitignore", "tracked.txt", ...Object.keys(files), ...uiFiles]);
  git(["commit", "--quiet", "--no-gpg-sign", "-m", "baseline"]);
  const head = git(["rev-parse", "--verify", "HEAD^{commit}"]).trim();
  const admin = git(["rev-parse", "--absolute-git-dir"]).trim();
  return { root, env, git, head, admin };
}

async function hosted(t, callback, files, options) {
  const repository = gitRepository(t, files, options);
  await withHostedRegistrationFixture(repository.root, repository.env,
    (env) => callback({ ...repository, env }));
}

const executed = (admin) => existsSync(join(admin, marker));

test("the launcher enables automatic input selection independently of the host", () => {
  assert.equal(ORDINARY_COMMIT_INPUT_DEFAULT, true);
  assert.match(readFileSync(join(sourceRoot, "src/ordinary_commit_input.rs"), "utf8"), /ORDINARY_COMMIT_INPUT_DEFAULT: bool = false/u);
});

test("a clean commit records its commit, stays clean and is eligible when stages pass", async (t) => {
  await hosted(t, async ({ root, env, head, admin }) => {
    const runDir = await createRun({ root, stages: [markingStage(admin)], input: "commit" }, env);
    assert.equal(json(join(runDir, "request.json")).input, "commit");
    const saved = json(join(runDir, "results.json"));
    assert.deepEqual(saved.input, { kind: "commit", commit: head, cleanBefore: true });
    assert.match(head, /^[0-9a-f]{40}$/u);
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "passed", result.error);
    assert.ok(executed(admin));
    assert.equal(result.input.outcome, "clean");
    assert.equal(result.input.eligible, true);
    assert.deepEqual(result.input.after, { commit: head, clean: true, paths: [] });
    assert.equal(result.before, result.after);
    assert.match(await summarize(runDir), new RegExp(`^input: commit ${head} clean; eligible$`, "mu"));
  });
});

for (const full of [false, true]) {
  test(`automatic clean ${full ? "full" : "focused"} admission is chosen before immutable registration`, async (t) => {
    const { root, env, head, admin } = gitRepository(t, {}, { full });
    await registrationHost(async (host) => {
      let captures = 0;
      const runDir = await createRun({ root, full, stages: [markingStage(admin)], automaticInput: true,
        commitInputStatus: (...args) => { captures += 1; return captureCommitInputStatus(...args); } },
        { ...env, TERMAL_BASE_URL: host.url });
      assert.equal(captures, 2, "selection reuses the initial status; only the final recheck follows it");
      const runId = json(join(runDir, "request.json")).runId;
      assert.equal(host.registrations.get(runId).request.input, "commit");
      const result = await executeRun(runDir, { ...env, TERMAL_BASE_URL: host.url }, { diskSpace: capacity });
      if (full) assert.equal(result.preflight.find((entry) => entry.name === "ui-dependencies")?.state, "passed");
      assert.equal(result.state, "passed", result.error);
      assert.equal(result.input.commit, head);
      assert.equal(result.input.eligible, true);
      assert.equal(result.before, result.after);
      assert.doesNotMatch(await summarize(runDir), /^gate: not eligible/mu);
      assert.ok(executed(admin));
    }, { autoAcknowledge: true });
  });
}

for (const [full, code] of [[false, 0], [true, 0], [false, 7], [true, 7]]) {
  test(`automatic ${full ? "full" : "focused"} input preserves exit ${code} when post-run admission is unavailable`, async (t) => {
    await hosted(t, async ({ root, env, head, admin }) => {
      const headPath = join(admin, "HEAD");
      const savedHead = join(admin, "fixture-saved-HEAD");
      // Keep a valid Git directory so discovery cannot fall through to this
      // fixture's parent repository. Only the requested commit is unavailable.
      const makeUnavailable = `const fs=require('node:fs');fs.copyFileSync(${JSON.stringify(headPath)}, ${JSON.stringify(savedHead)});fs.writeFileSync(${JSON.stringify(headPath)}, 'ref: refs/heads/fixture-unavailable\\n');process.exit(${code});`;
      const runDir = await createRun({ root, full, stages: [markingStage(admin, "unavailable-after", makeUnavailable)],
        automaticInput: true }, env);
      assert.equal(json(join(runDir, "request.json")).input, "commit");
      try {
        const result = await executeRun(runDir, env, { diskSpace: capacity });
        if (full) assert.equal(result.preflight.find((entry) => entry.name === "ui-dependencies")?.state, "passed");
        assert.ok(executed(admin), "the command must actually run");
        assert.equal(result.stages[0].state, code === 0 ? "passed" : "failed");
        assert.equal(result.stages[0].code, code);
        assert.equal(result.state, code === 0 ? "passed" : "failed", result.error);
        assert.equal(result.exitCode, code);
        assert.equal(result.input.commit, head);
        assert.equal(result.input.outcome, undefined);
        assert.equal(result.input.eligible, false);
        assert.equal(result.input.after, undefined);
        assert.equal(result.after, undefined);
        assert.match(result.input.unavailableReason, /post-run input unavailable: git /u);
        assert.deepEqual(json(join(runDir, "results.json")).input, result.input);
        assert.match(await summarize(runDir), /no post-run input recorded; not eligible/u);
        assert.match(await summarize(runDir), /^gate: not eligible; not a landing gate: .+$/mu);
      } finally {
        if (existsSync(savedHead)) renameSync(savedHead, headPath);
      }
    }, {}, { full });
  });
}

test("automatic registration refusal preserves its cause and names explicit plain without downgrading", async (t) => {
  const { root, env, admin } = gitRepository(t);
  await registrationHost(async (host) => {
    const hostEnv = { ...env, TERMAL_BASE_URL: host.url };
    let refusedDir;
    await assert.rejects(createRun({ root, stages: [markingStage(admin)], automaticInput: true,
      onRunCreated: (directory) => { refusedDir = directory; } }, hostEnv), (error) => {
      assert.match(error.message, /exact held claim is unavailable or ambiguous/u);
      assert.match(error.message, /renew the claim or name the source root again/u);
      assert.match(error.message, /--input plain/u);
      return true;
    });
    assert.equal(host.requests.length, 1, "a confirmed registration refusal never changes or retries the input kind");
    assert.equal(json(join(refusedDir, "request.json")).input, "commit");
    const refused = json(join(refusedDir, "results.json"));
    assert.match(refused.error, /exact held claim is unavailable or ambiguous/u);
    assert.ok(refused.stages.every((entry) => entry.state === "unrun"));
    assert.equal(executed(admin), false);

    // The user explicitly chooses another run; the failed committed request
    // is never rewritten or silently turned into a plain registration.
    const plainDir = await createRun({ root, input: "plain", automaticInput: true,
      stages: [markingStage(admin)] }, hostEnv);
    const request = json(join(plainDir, "request.json"));
    assert.equal(Object.hasOwn(host.registrations.get(request.runId).request, "input"), false);
    const result = await executeRun(plainDir, hostEnv);
    assert.equal(result.state, "passed", result.error);
    assert.equal(result.exitCode, 0);
    assert.equal(result.input, undefined);
    assert.ok(executed(admin));
    assert.equal(host.requests.length, 2);
    assert.notEqual(plainDir, refusedDir);
  }, { autoAcknowledge: true,
    status: (record) => json(join(record.body.runDirectory, "request.json")).input === "commit" ? 409 : 201,
    errorReason: "named run's exact held claim is unavailable or ambiguous" });
});

for (const code of [0, 7]) {
  test(`an automatic full run preserves command exit ${code} but rejects a changed input as a landing gate`, async (t) => {
    await hosted(t, async ({ root, env, admin }) => {
      const edit = `require('node:fs').writeFileSync('tracked.txt', 'changed during full');process.exit(${code});`;
      const runDir = await createRun({ root, full: true, automaticInput: true,
        stages: [markingStage(admin, "edit", edit)] }, env);
      assert.equal(json(join(runDir, "request.json")).full, true);
      const result = await executeRun(runDir, env, { diskSpace: capacity });
      assert.equal(result.preflight.find((entry) => entry.name === "ui-dependencies")?.state, "passed");
      assert.ok(executed(admin));
      assert.equal(result.stages[0].code, code);
      assert.equal(result.stages[0].state, code === 0 ? "passed" : "failed");
      assert.equal(result.state, code === 0 ? "passed" : "failed");
      assert.equal(result.exitCode, code);
      assert.equal(result.input.outcome, "input_changed");
      assert.equal(result.input.eligible, false);
      assert.notEqual(result.before, result.after);
      const summary = await summarize(runDir);
      assert.match(summary, new RegExp(`^${code === 0 ? "PASS" : "FAIL"} .+ exit=${code}$`, "mu"));
      assert.match(summary, /^gate: not eligible; not a landing gate: 1 path\(s\) reported by git status$/mu);
    }, {}, { full: true });
  });
}

test("automatic plain reasons stay bounded on one summary line", () => {
  const reason = `fixture refused\n${"warning\r\n".repeat(10000)}`;
  const line = commitInputSummaryLine({ inputSelection: { kind: "plain", reason } });
  assert.match(line, /^input: plain; no committed-input verification: fixture refused /u);
  assert.equal(line.split(/[\r\n]/u).length, 1);
  assert.ok(line.length < 2500, "metadata and stderr cannot make the summary unbounded");
  assert.ok(line.endsWith("..."));
});

for (const [name, dirty] of [
  ["tracked", (root) => writeFileSync(join(root, "tracked.txt"), "ordinary work\n")],
  ["untracked", (root) => writeFileSync(join(root, "new.txt"), "ordinary work\n")],
]) {
  for (const code of [0, 7]) {
    test(`automatic ${name} input runs plainly and preserves command exit ${code}`, async (t) => {
      const { root, env, admin } = gitRepository(t);
      dirty(root);
      await registrationHost(async (host) => {
        const stage = markingStage(admin);
        stage.args[1] += `; process.exit(${code})`;
        const runDir = await createRun({ root, stages: [stage], automaticInput: true },
          { ...env, TERMAL_BASE_URL: host.url });
        const request = json(join(runDir, "request.json"));
        assert.equal(Object.hasOwn(host.registrations.get(request.runId).request, "input"), false,
          "the host receives a plain request before acknowledging registration");
        const result = await executeRun(runDir, { ...env, TERMAL_BASE_URL: host.url });
        assert.ok(executed(admin));
        assert.equal(result.state, code === 0 ? "passed" : "failed", result.error);
        assert.equal(result.exitCode, code);
        assert.equal(result.stages[0].code, code);
        assert.equal(result.input, undefined, "a plain run supplies no committed-input verification record");
        assert.equal(result.inputSelection.kind, "plain");
        assert.ok(result.inputSelection.reason);
        assert.match(await summarize(runDir), /^input: plain; no committed-input verification: /mu);
        assert.equal(result.before, result.after, "stable dirty input keeps the existing fingerprint guard");
      }, { autoAcknowledge: true });
    });
  }
}

for (const [name, configure, reason] of [
  ["skipped tracked input", ({ root, git }) => {
    git(["update-index", "--skip-worktree", "tracked.txt"]);
    writeFileSync(join(root, "tracked.txt"), "hidden ordinary work\n");
  }, /Git is told to skip/u],
  ["unavailable admission", () => {}, /fixture admission unavailable/u],
]) {
  test(`automatic ${name} keeps the command and names the plain selection`, async (t) => {
    await hosted(t, async (repository) => {
      const { root, env, admin, head } = repository;
      configure(repository);
      const runDir = await createRun({ root, stages: [markingStage(admin)], automaticInput: true,
        ...(name === "unavailable admission" ? { commitInputStatus: async () => { throw new Error("fixture admission unavailable"); } } : {}) }, env);
      assert.equal(Object.hasOwn(json(join(runDir, "request.json")), "input"), false);
      const result = await executeRun(runDir, env);
      assert.ok(executed(admin));
      assert.equal(result.state, "passed", result.error);
      assert.equal(result.exitCode, 0);
      assert.equal(result.input, undefined);
      assert.match(result.inputSelection.reason, reason);
      assert.match(await summarize(runDir), reason);
    });
  });
}

test("a clean checkout with a gitlink runs plainly and explicit commit still refuses it", async (t) => {
  const dependency = gitRepository(t);
  await hosted(t, async ({ root, env, git, admin }) => {
    git(["-c", "protocol.file.allow=always", "submodule", "add", "--quiet", dependency.root, "dep"]);
    git(["commit", "--quiet", "--no-gpg-sign", "-m", "pin dependency"]);
    assert.equal(git(["status", "--porcelain=v1", "--ignore-submodules=none"]), "");
    const runDir = await createRun({ root, automaticInput: true, stages: [markingStage(admin)] }, env);
    assert.equal(Object.hasOwn(json(join(runDir, "request.json")), "input"), false);
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "passed", result.error);
    assert.ok(executed(admin));
    assert.equal(result.input, undefined);
    assert.match(await summarize(runDir), /^input: plain; no committed-input verification: 1 submodule gitlink\(s\) \(dep\)/mu);
    const strictDir = await createRun({ root, input: "commit", stages: [stage("strict")] }, env);
    const refused = await executeRun(strictDir, env);
    assert.equal(refused.state, "failed");
    assert.deepEqual(refused.input.gitlinks, ["dep"]);
    assert.ok(refused.stages.every((entry) => entry.state === "unrun"));
  });
});

test("automatic fallback does not bypass plain's readable HEAD precondition", async (t) => {
  await hosted(t, async ({ root, env, git, admin }) => {
    const original = git(["symbolic-ref", "HEAD"]).trim();
    git(["symbolic-ref", "HEAD", "refs/heads/unborn-fixture"]);
    try {
      const runDir = await createRun({ root, automaticInput: true, stages: [markingStage(admin)] }, env);
      assert.equal(Object.hasOwn(json(join(runDir, "request.json")), "input"), false);
      const result = await executeRun(runDir, env);
      assert.equal(result.state, "failed");
      assert.match(result.error, /HEAD/u);
      assert.equal(executed(admin), false);
      assert.ok(result.stages.every((entry) => entry.state === "unrun"));
    } finally {
      git(["symbolic-ref", "HEAD", original]);
    }
  });
});

test("an automatic plain run retains the existing post-run fingerprint guard", async (t) => {
  await hosted(t, async ({ root, env, admin }) => {
    writeFileSync(join(root, "tracked.txt"), "dirty before\n");
    const edit = `require('node:fs').writeFileSync(${JSON.stringify(join(root, "tracked.txt"))}, 'changed during');`;
    const runDir = await createRun({ root, stages: [markingStage(admin, "edit", edit)], automaticInput: true }, env);
    const result = await executeRun(runDir, env);
    assert.ok(executed(admin));
    assert.equal(result.stages[0].state, "passed");
    assert.equal(result.stages[0].code, 0);
    assert.equal(result.state, "failed");
    assert.match(result.error, /input drift/u);
    assert.equal(result.input, undefined);
  });
});

for (const [name, dirty, expected] of [
  ["a tracked edit", (root) => writeFileSync(join(root, "tracked.txt"), "edited\n"), { status: " M", path: "tracked.txt" }],
  ["an untracked file", (root) => writeFileSync(join(root, "new.txt"), "new\n"), { status: "??", path: "new.txt" }],
]) {
  test(`a checkout dirty with ${name} before the run is refused and no stage runs`, async (t) => {
    await hosted(t, async ({ root, env, admin }) => {
      dirty(root);
      const runDir = await createRun({ root, stages: [markingStage(admin)], input: "commit" }, env);
      const refused = json(join(runDir, "results.json"));
      assert.equal(refused.state, "failed");
      assert.equal(refused.exitCode, 1);
      assert.deepEqual(refused.input, { kind: "commit", cleanBefore: false, dirtyPaths: [expected] });
      assert.match(refused.error, /not clean before the run.*no stage ran/u);
      const result = await executeRun(runDir, env);
      assert.equal(result.state, "failed");
      assert.notEqual(result.exitCode, 0);
      assert.ok(result.stages.every((entry) => entry.state === "unrun"));
      assert.equal(executed(admin), false);
      assert.match(await summarize(runDir), /^input: refused, not clean before the run: 1 path\(s\)$/mu);
    });
  });
}

test("a path the repository's .gitignore ignores before the run leaves the checkout clean", async (t) => {
  await hosted(t, async ({ root, env, head, admin }) => {
    writeFileSync(join(root, "ignored-before.log"), "ignored\n");
    const runDir = await createRun({ root, stages: [markingStage(admin)], input: "commit" }, env);
    assert.deepEqual(json(join(runDir, "results.json")).input, { kind: "commit", commit: head, cleanBefore: true });
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "passed", result.error);
    assert.equal(result.input.outcome, "clean");
    assert.equal(result.input.eligible, true);
  });
});

test("a path only .git/info/exclude hides before the run leaves the checkout clean", async (t) => {
  await hosted(t, async ({ root, env, head, admin }) => {
    writeFileSync(join(root, "excluded.txt"), "excluded\n");
    const runDir = await createRun({ root, stages: [markingStage(admin)], input: "commit" }, env);
    assert.deepEqual(json(join(runDir, "results.json")).input, { kind: "commit", commit: head, cleanBefore: true });
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "passed", result.error);
    assert.equal(result.input.outcome, "clean");
    assert.equal(result.input.eligible, true);
  });
});

test("only repository-scoped excludes leave an untracked path clean; excludes files do not", async (t) => {
  const { root, env, git, head } = gitRepository(t);
  const sorted = (entries) => [...entries].sort((left, right) => left.path.localeCompare(right.path));
  // Git's own status hides each of these; the committed-input status hides
  // only the .gitignore and .git/info/exclude ones.
  const xdg = join(root, ".tmp", "xdg");
  mkdirSync(join(xdg, "git"), { recursive: true });
  writeFileSync(join(xdg, "git", "ignore"), "global-default.txt\n");
  const configured = join(root, ".tmp", "configured-excludes");
  writeFileSync(configured, "configured.txt\n");
  git(["config", "core.excludesFile", configured]);
  const xdgEnv = { ...env, XDG_CONFIG_HOME: xdg };
  writeFileSync(join(root, "excluded.txt"), "info/exclude\n");
  writeFileSync(join(root, "global-default.txt"), "default global excludes file\n");
  writeFileSync(join(root, "configured.txt"), "configured excludes file\n");
  writeFileSync(join(root, "ignored-by-gitignore.log"), "ignored\n");
  writeFileSync(join(root, "ignored-also"), ".gitignore\n");
  const plain = (...paths) => execFileSync("git",
    ["status", "--porcelain=v1", "--untracked-files=all", "--", ...paths],
    { cwd: root, env: xdgEnv, encoding: "utf8", windowsHide: true });
  const both = [
    { status: "??", path: "configured.txt" },
    { status: "??", path: "global-default.txt" },
  ];
  // A repo-local excludes file (which replaces the default one).
  assert.equal(plain("configured.txt", "excluded.txt", "ignored-by-gitignore.log", "ignored-also"), "",
    "plain status hides the info/exclude, configured excludes file and .gitignore paths");
  const status = await captureCommitInputStatus(root, xdgEnv);
  assert.equal(status.commit, head);
  assert.deepEqual(status.skipped, []);
  assert.deepEqual(sorted(status.entries), both);
  assert.equal(commitInputStatusClean(status), false);
  // Without one, the default excludes file under XDG_CONFIG_HOME.
  git(["config", "--unset", "core.excludesFile"]);
  assert.equal(plain("global-default.txt", "excluded.txt"), "",
    "plain status hides the default excludes file's path");
  assert.deepEqual(sorted((await captureCommitInputStatus(root, xdgEnv)).entries), both);
  rmSync(join(root, "configured.txt"));
  rmSync(join(root, "global-default.txt"));
  const clean = await captureCommitInputStatus(root, xdgEnv);
  assert.deepEqual(clean.entries, [], "info/exclude and .gitignore paths stay clean");
  assert.equal(commitInputStatusClean(clean), true);
});

test("an XDG attributes file does not change the committed-input status", async (t) => {
  const { root, env, git } = gitRepository(t);
  // Committed with CRLF endings: an attributes file that marks it text makes
  // Git normalize the worktree copy to LF and call it modified.
  writeFileSync(join(root, "crlf.txt"), "one\r\ntwo\r\n");
  git(["add", "--", "crlf.txt"]);
  git(["commit", "--quiet", "--no-gpg-sign", "-m", "crlf"]);
  const head = git(["rev-parse", "--verify", "HEAD^{commit}"]).trim();
  const xdg = join(root, ".tmp", "xdg-attributes");
  mkdirSync(join(xdg, "git"), { recursive: true });
  writeFileSync(join(xdg, "git", "attributes"), "crlf.txt text eol=lf\n");
  const xdgEnv = { ...env, XDG_CONFIG_HOME: xdg };
  // A new mtime makes Git read the file again rather than trust the index.
  const later = new Date(Date.now() + 60_000);
  utimesSync(join(root, "crlf.txt"), later, later);
  const plain = execFileSync("git", ["status", "--porcelain=v1", "--", "crlf.txt"],
    { cwd: root, env: xdgEnv, encoding: "utf8", windowsHide: true });
  assert.equal(plain, " M crlf.txt\n", "plain status applies the XDG attributes file");
  const status = await captureCommitInputStatus(root, xdgEnv);
  assert.equal(status.commit, head);
  assert.deepEqual(status.entries, [], "the committed-input status ignores it");
  assert.deepEqual(status.skipped, []);
  assert.equal(commitInputStatusClean(status), true);
});

for (const [flag, tag] of [["--skip-worktree", "S"], ["--assume-unchanged", "h"]]) {
  test(`a tracked path marked ${flag} is not clean`, async (t) => {
    const { root, env, git } = gitRepository(t);
    git(["update-index", flag, "tracked.txt"]);
    writeFileSync(join(root, "tracked.txt"), "edited behind the flag\n");
    assert.equal(git(["status", "--porcelain=v1"]), "", "plain status calls the edit clean");
    const status = await captureCommitInputStatus(root, env);
    assert.deepEqual(status.entries, []);
    assert.deepEqual(status.skipped, [{ tag, path: "tracked.txt" }]);
    assert.equal(commitInputStatusClean(status), false);
    assert.equal(commitInputOutcome(status.commit, status), "input_changed");
  });
}

test("a skip-worktree path before the run is refused with the skipped path and no stage runs", async (t) => {
  await hosted(t, async ({ root, env, git, admin }) => {
    git(["update-index", "--skip-worktree", "tracked.txt"]);
    writeFileSync(join(root, "tracked.txt"), "edited behind the flag\n");
    const runDir = await createRun({ root, stages: [markingStage(admin)], input: "commit" }, env);
    const refused = json(join(runDir, "results.json"));
    assert.equal(refused.state, "failed");
    assert.deepEqual(refused.input, {
      kind: "commit", cleanBefore: false, dirtyPaths: [],
      skippedPaths: [{ tag: "S", path: "tracked.txt" }],
    });
    assert.match(refused.error, /1 tracked path\(s\) Git is told to skip.*no stage ran/u);
    const result = await executeRun(runDir, env);
    assert.ok(result.stages.every((entry) => entry.state === "unrun"));
    assert.equal(executed(admin), false);
    assert.match(await summarize(runDir),
      /^input: refused, not clean before the run: 0 path\(s\); 1 skipped tracked path\(s\)$/mu);
  });
});

test("a stage that marks a tracked path skip-worktree changes the input", async (t) => {
  await hosted(t, async ({ root, env, head }) => {
    const runDir = await createRun({ root, input: "commit", stages: [stage("skips", [
      "const { execFileSync } = require('node:child_process');",
      "execFileSync('git', ['update-index', '--skip-worktree', 'tracked.txt']);",
    ].join("\n"))] }, env);
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "passed", result.error);
    assert.equal(result.input.outcome, "input_changed");
    assert.equal(result.input.eligible, false);
    assert.deepEqual(result.input.after, {
      commit: head, clean: false, paths: [], skippedPaths: [{ tag: "S", path: "tracked.txt" }],
    });
  });
});

test("a stage that configures a Git filter changes the input and records the filter", async (t) => {
  await hosted(t, async ({ root, env, head }) => {
    const runDir = await createRun({ root, input: "commit", stages: [stage("filters", [
      "const { execFileSync } = require('node:child_process');",
      "execFileSync('git', ['config', 'filter.fixture.clean', 'cat']);",
    ].join("\n"))] }, env);
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "passed", result.error);
    assert.equal(result.input.outcome, "input_changed");
    assert.equal(result.input.eligible, false);
    assert.deepEqual(result.input.after, {
      commit: head, clean: false, paths: [], filters: ["filter.fixture.clean"],
    });
  });
});

test("every committed-input Git call turns fsmonitor off, as the host's runner does", () => {
  const nullDevice = process.platform === "win32" ? "NUL" : "/dev/null";
  assert.deepEqual(commitInputGitArgs(["status", "-z"]), [
    "-c", "core.fsmonitor=false",
    "-c", `core.excludesFile=${nullDevice}`,
    "-c", `core.attributesFile=${nullDevice}`,
    "status", "-z",
  ]);
});

test("a configured Git filter is not clean and its status is not read", async (t) => {
  const { root, env, git, head } = gitRepository(t);
  git(["config", "filter.fixture.clean", "cat"]);
  git(["config", "filter.fixture.smudge", "cat"]);
  // An edit Git's status would report is not read under a filter.
  writeFileSync(join(root, "tracked.txt"), "edited under a filter\n");
  const status = await captureCommitInputStatus(root, env);
  assert.equal(status.commit, head);
  assert.deepEqual(status.entries, []);
  assert.deepEqual(status.skipped, []);
  assert.deepEqual([...status.filters].sort(), ["filter.fixture.clean", "filter.fixture.smudge"]);
  assert.equal(commitInputStatusClean(status), false);
  assert.equal(commitInputOutcome(head, status), "input_changed");
  // Without a clean, smudge or process command the driver is not a filter.
  git(["config", "--unset", "filter.fixture.clean"]);
  git(["config", "--unset", "filter.fixture.smudge"]);
  git(["config", "filter.fixture.required", "true"]);
  writeFileSync(join(root, "tracked.txt"), "baseline\n");
  const clean = await captureCommitInputStatus(root, env);
  assert.deepEqual(clean.filters, []);
  assert.equal(commitInputStatusClean(clean), true);
});

test("a configured Git filter before the run is refused with its own reason and no stage runs", async (t) => {
  await hosted(t, async ({ root, env, git, admin }) => {
    git(["config", "filter.fixture.process", "cat"]);
    const runDir = await createRun({ root, stages: [markingStage(admin)], input: "commit" }, env);
    const refused = json(join(runDir, "results.json"));
    assert.equal(refused.state, "failed");
    assert.deepEqual(refused.input, {
      kind: "commit", cleanBefore: false, dirtyPaths: [], filters: ["filter.fixture.process"],
    });
    assert.match(refused.error,
      /not clean before the run \(1 configured Git filter\(s\) \(filter\.fixture\.process\).*\); no stage ran/u);
    const result = await executeRun(runDir, env);
    assert.ok(result.stages.every((entry) => entry.state === "unrun"));
    assert.equal(executed(admin), false);
    assert.match(await summarize(runDir),
      /^input: refused, not clean before the run: 0 path\(s\); 1 configured Git filter\(s\)$/mu);
  });
});

test("a submodule gitlink is refused unread and its own filter never runs", async (t) => {
  await hosted(t, async ({ root, env, git, admin }) => {
    // The submodule configures a clean filter in its own config, which the
    // superproject's filter check does not see; the filter leaves a marker.
    const sub = join(root, "sub");
    mkdirSync(sub);
    const subGit = (args) => execFileSync("git", args, { cwd: sub, env, encoding: "utf8", windowsHide: true });
    subGit(["init", "--quiet", "--template="]);
    for (const [key, value] of [
      ["user.name", "TermAl Test"], ["user.email", "test@example.invalid"], ["core.autocrlf", "false"],
    ]) subGit(["config", key, value]);
    const filterRan = join(root, ".tmp", "submodule-filter-ran.marker");
    const script = join(root, ".tmp", "submodule-filter.js");
    mkdirSync(dirname(script), { recursive: true });
    writeFileSync(script, `require("node:fs").appendFileSync(${JSON.stringify(filterRan)}, "x");`
      + "process.stdin.pipe(process.stdout);\n");
    const slash = (path) => path.replaceAll("\\", "/");
    writeFileSync(join(sub, ".gitattributes"), "inner.txt filter=mark\n");
    writeFileSync(join(sub, "inner.txt"), "inner\n");
    subGit(["add", "--", ".gitattributes", "inner.txt"]);
    subGit(["commit", "--quiet", "--no-gpg-sign", "-m", "inner"]);
    subGit(["config", "filter.mark.clean", `"${slash(process.execPath)}" "${slash(script)}"`]);
    writeFileSync(join(root, ".gitmodules"), "[submodule \"sub\"]\n\tpath = sub\n\turl = ./sub\n");
    git(["-c", "advice.addEmbeddedRepo=false", "add", "--", ".gitmodules", "sub"]);
    git(["commit", "--quiet", "--no-gpg-sign", "-m", "submodule"]);
    assert.equal(git(["ls-files", "-s", "sub"]).slice(0, 6), "160000", "sub is a gitlink");
    // A new mtime makes Git hash the file again, through the filter.
    let seconds = 60;
    const touch = () => {
      const later = new Date(Date.now() + (seconds += 60) * 1000);
      utimesSync(join(sub, "inner.txt"), later, later);
    };
    touch();
    git(["status", "--porcelain=v1", "--ignore-submodules=none"]);
    assert.ok(existsSync(filterRan), "plain status runs the submodule's filter");
    rmSync(filterRan);
    touch();

    const status = await captureCommitInputStatus(root, env);
    assert.deepEqual(status.entries, []);
    assert.deepEqual(status.gitlinks, ["sub"]);
    assert.equal(commitInputStatusClean(status), false);
    assert.equal(commitInputOutcome(status.commit, status), "input_changed");

    const runDir = await createRun({ root, stages: [markingStage(admin)], input: "commit" }, env);
    const refused = json(join(runDir, "results.json"));
    assert.equal(refused.state, "failed");
    assert.deepEqual(refused.input, {
      kind: "commit", cleanBefore: false, dirtyPaths: [], gitlinks: ["sub"],
    });
    assert.match(refused.error, /not clean before the run \(1 submodule gitlink\(s\) \(sub\).*\); no stage ran/u);
    const result = await executeRun(runDir, env);
    assert.ok(result.stages.every((entry) => entry.state === "unrun"));
    assert.equal(executed(admin), false);
    assert.match(await summarize(runDir),
      /^input: refused, not clean before the run: 0 path\(s\); 1 submodule gitlink\(s\)$/mu);
    assert.equal(existsSync(filterRan), false, "the committed-input status never ran the filter");
  });
});

test("an edit made while the input is captured refuses the run as not clean before it", async (t) => {
  await hosted(t, async ({ root, env, admin }) => {
    let calls = 0;
    const commitInputStatus = async (...args) => {
      calls += 1;
      const status = await captureCommitInputStatus(...args);
      // The edit lands after the first read, before the fingerprint.
      if (calls === 1) writeFileSync(join(root, "tracked.txt"), "edited in the window\n");
      return status;
    };
    const runDir = await createRun(
      { root, stages: [markingStage(admin)], input: "commit", commitInputStatus }, env);
    assert.equal(calls, 2, "the status is read again after the fingerprint");
    const refused = json(join(runDir, "results.json"));
    assert.equal(refused.state, "failed");
    assert.deepEqual(refused.input, {
      kind: "commit", cleanBefore: false, dirtyPaths: [{ status: " M", path: "tracked.txt" }],
    });
    assert.match(refused.error, /changed while the input was captured.*no stage ran/u);
    // Reverting the edit afterwards does not revive the run.
    writeFileSync(join(root, "tracked.txt"), "baseline\n");
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "failed");
    assert.ok(result.stages.every((entry) => entry.state === "unrun"));
    assert.equal(executed(admin), false);
  });
});

test("a stage that leaves a new untracked file is debris pending disposition, not eligible", async (t) => {
  await hosted(t, async ({ root, env, head, admin }) => {
    const runDir = await createRun({ root, input: "commit", stages: [markingStage(admin, "fixture",
      "require('node:fs').writeFileSync('debris.txt', 'left behind'); require('node:fs').writeFileSync('ignored-debris.log', 'ignored');")] }, env);
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "passed", result.error);
    assert.equal(result.stages[0].state, "passed");
    assert.equal(result.error, undefined);
    assert.notEqual(result.after, result.before, "the fingerprint still records the drift");
    assert.equal(result.input.outcome, "debris_pending_disposition");
    assert.equal(result.input.disposition, null);
    assert.equal(result.input.eligible, false);
    assert.deepEqual(result.input.after, { commit: head, clean: false, paths: [{ status: "??", path: "debris.txt" }] });
    assert.deepEqual(json(join(runDir, "results.json")).input, result.input);
    assert.match(await summarize(runDir),
      new RegExp(`^input: commit ${head} debris \\(pending disposition\\): 1 new untracked path\\(s\\); not eligible$`, "mu"));
  });
});

test("a stage that edits a tracked file changes the input, keeps stage results and is not eligible", async (t) => {
  await hosted(t, async ({ root, env, head, admin }) => {
    const runDir = await createRun({ root, input: "commit", stages: [
      markingStage(admin, "edits", "require('node:fs').writeFileSync('tracked.txt', 'changed during execution\\n');"),
      stage("second"),
    ] }, env);
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "passed", "the run's own state is kept");
    assert.deepEqual(result.stages.map(({ state }) => state), ["passed", "passed"]);
    assert.equal(result.error, undefined);
    assert.equal(result.input.outcome, "input_changed");
    assert.equal(result.input.eligible, false);
    assert.equal("disposition" in result.input, false);
    assert.deepEqual(result.input.after.paths, [{ status: " M", path: "tracked.txt" }]);
    assert.match(await summarize(runDir), new RegExp(`^input: commit ${head} input changed; not eligible$`, "mu"));
  });
});

test("a stage that commits moves HEAD and changes the input", async (t) => {
  await hosted(t, async ({ root, env, head }) => {
    const runDir = await createRun({ root, input: "commit", stages: [stage("commits", [
      "const { execFileSync } = require('node:child_process');",
      "require('node:fs').writeFileSync('tracked.txt', 'committed during execution\\n');",
      "execFileSync('git', ['commit', '--quiet', '--no-gpg-sign', '-am', 'during']);",
    ].join("\n"))] }, env);
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "passed", result.error);
    assert.equal(result.input.outcome, "input_changed");
    assert.notEqual(result.input.after.commit, head);
    assert.equal(result.input.after.clean, true);
    assert.equal(result.input.eligible, false);
  });
});

test("a failing stage on a clean commit is clean but not eligible", async (t) => {
  await hosted(t, async ({ root, env }) => {
    const runDir = await createRun({ root, input: "commit", stages: [stage("fails", "process.exit(7)")] }, env);
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "failed");
    assert.equal(result.exitCode, 7);
    assert.equal(result.input.outcome, "clean");
    assert.equal(result.input.eligible, false);
  });
});

test("without the selection the staged lane is unchanged and drift still throws", async (t) => {
  await hosted(t, async ({ root, env }) => {
    writeFileSync(join(root, "new.txt"), "untracked inputs belong to the staged lane\n");
    const runDir = await createRun({ root, stages: [stage(
      "changes-source",
      "require('node:fs').writeFileSync('tracked.txt', 'changed during execution\\n')",
    )] }, env);
    assert.equal("input" in json(join(runDir, "request.json")), false);
    const result = await executeRun(runDir, env);
    assert.equal(result.state, "failed");
    assert.match(result.error, /input drift: results do not validate the current source/u);
    assert.equal(result.input, undefined);
    assert.doesNotMatch(await summarize(runDir), /^input:/mu);
  });
});

test("status entries and outcomes classify renames, staged additions and debris", async (t) => {
  const { root, env, git, head } = gitRepository(t);
  git(["mv", "tracked.txt", "renamed.txt"]);
  writeFileSync(join(root, "added.txt"), "added\n");
  git(["add", "--", "added.txt"]);
  writeFileSync(join(root, "loose.txt"), "loose\n");
  const status = await captureCommitInputStatus(root, env);
  assert.equal(status.commit, head);
  assert.deepEqual(status.entries.sort((left, right) => left.path.localeCompare(right.path)), [
    { status: "A ", path: "added.txt" },
    { status: "??", path: "loose.txt" },
    { status: "R ", path: "renamed.txt", origin: "tracked.txt" },
  ]);
  assert.equal(commitInputOutcome(head, status), "input_changed");
  assert.equal(commitInputOutcome(head, { commit: head, entries: [{ status: "??", path: "x" }] }),
    "debris_pending_disposition");
  assert.equal(commitInputOutcome(head, { commit: head, entries: [] }), "clean");
  assert.equal(commitInputOutcome(head, { commit: "0".repeat(40), entries: [] }), "input_changed");
  assert.equal(commitInputOutcome(head, {
    commit: head, entries: [], skipped: [{ tag: "S", path: "tracked.txt" }],
  }), "input_changed");
});

// The command line selects before registration. Explicit `--input commit`
// stays strict; other `--input` values are refused before a run exists.
test("the command line selects automatically, preserves strict --input commit and refuses other values", async (t) => {
  const files = Object.fromEntries(helperSupportFiles.map((path) => [path, join(sourceRoot, path)]));
  const { root, env, admin } = gitRepository(t, files);
  const cli = (args, hostEnv) => new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [join(root, "scripts", "test-launcher.mjs"), ...args],
      { cwd: root, env: hostEnv, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", reject);
    child.once("close", (code) => resolve({ code, stdout, stderr }));
  });
  const invalid = await cli(["focused", "--input", "staged", "--", process.execPath, "-e", ""], env);
  assert.equal(invalid.code, 1);
  assert.match(invalid.stderr, /invalid option: --input/u);
  assert.equal(existsSync(join(admin, "review-runs")), false);
  await registrationHost(async (host) => {
    const hostEnv = { ...env, TERMAL_BASE_URL: host.url };
    const head = execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, env, encoding: "utf8" }).trim();
    const passed = await cli(["focused", "--input", "commit", "--", process.execPath, "-e", ""], hostEnv);
    assert.equal(passed.code, 0, `${passed.stdout}\n${passed.stderr}`);
    const runDir = /^RUN (.+)$/mu.exec(passed.stdout)[1].trim();
    assert.equal(json(join(runDir, "request.json")).input, "commit");
    assert.match(passed.stdout, new RegExp(`^input: commit ${head} clean; eligible$`, "mu"));

    const automatic = await cli(["focused", "--", process.execPath, "-e", ""], hostEnv);
    assert.equal(automatic.code, 0, `${automatic.stdout}\n${automatic.stderr}`);
    const automaticDir = /^RUN (.+)$/mu.exec(automatic.stdout)[1].trim();
    const automaticRequest = json(join(automaticDir, "request.json"));
    assert.equal(host.registrations.get(automaticRequest.runId).request.input, "commit");
    assert.equal(json(join(automaticDir, "results.json")).input.eligible, true);

    const plainClean = await cli(["focused", "--input", "plain", "--", process.execPath,
      "-e", markingStage(admin).args[1]], hostEnv);
    assert.equal(plainClean.code, 0, `${plainClean.stdout}\n${plainClean.stderr}`);
    const plainCleanDir = /^RUN (.+)$/mu.exec(plainClean.stdout)[1].trim();
    const plainCleanRequest = json(join(plainCleanDir, "request.json"));
    assert.equal(Object.hasOwn(host.registrations.get(plainCleanRequest.runId).request, "input"), false);
    assert.equal(json(join(plainCleanDir, "results.json")).input, undefined);
    assert.ok(executed(admin), "the explicit plain command must execute on a clean checkout");
    assert.match(plainClean.stdout, /^input: plain; no committed-input verification: explicit --input plain$/mu);

    writeFileSync(join(root, "tracked.txt"), "dirty before the run\n");
    for (const code of [0, 9]) {
      const plain = await cli(["focused", "--", process.execPath, "-e", `process.exit(${code})`], hostEnv);
      assert.equal(plain.code, code, `${plain.stdout}\n${plain.stderr}`);
      const plainDir = /^RUN (.+)$/mu.exec(plain.stdout)[1].trim();
      const plainRequest = json(join(plainDir, "request.json"));
      assert.equal(Object.hasOwn(host.registrations.get(plainRequest.runId).request, "input"), false);
      const plainResult = json(join(plainDir, "results.json"));
      assert.equal(plainResult.stages[0].code, code);
      assert.equal(plainResult.input, undefined);
      assert.match(plain.stdout, /^input: plain; no committed-input verification: 1 path\(s\) reported by git status$/mu);
    }
    const refused = await cli(["focused", "--input", "commit", "--", process.execPath, "-e", ""], hostEnv);
    assert.equal(refused.code, 1, `${refused.stdout}\n${refused.stderr}`);
    assert.match(refused.stdout, /^input: refused, not clean before the run: 1 path\(s\)$/mu);
  }, { autoAcknowledge: true });
});
