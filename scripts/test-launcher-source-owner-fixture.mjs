// Test-only owner process for launcher integration tests. The launcher still
// runs the selected executable and validates the real v1 wire envelope.
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { isolatedGitEnvironment } from "./git-environment.mjs";

export function fixtureSourceEnvironment(env) {
  const preload = `--import=${import.meta.url}`;
  const nodeOptions = env.NODE_OPTIONS ?? "";
  return {
    ...env,
    ENGRAM_SOURCE_BINARY: process.execPath,
    NODE_OPTIONS: nodeOptions.split(/\s+/u).includes(preload) ? nodeOptions : `${nodeOptions} ${preload}`.trim(),
  };
}

export const fixtureObservationPath = (admin) => join(admin, "fixture-source-observation.json");

function observeFixture(root) {
  const git = (...args) => execFileSync("git", args, {
    cwd: root, env: isolatedGitEnvironment(process.env), encoding: "utf8",
    windowsHide: true, stdio: ["ignore", "pipe", "pipe"],
  });
  const refuse = (message) => ({
    version: 1, status: "unavailable", root, source: null, tree: null,
    entries: [], refusal: { code: "input_unavailable", message }, source_id: null,
  });
  let commit, tree, admin;
  try {
    [commit, tree, admin] = git("rev-parse", "HEAD", "HEAD^{tree}", "--absolute-git-dir").trimEnd().split(/\r?\n/u);
  } catch {
    return refuse("Fixture Git HEAD is unavailable");
  }
  if (commit.length !== 40) return refuse("Fixture owner supports SHA-1 roots only");
  const override = fixtureObservationPath(admin);
  if (existsSync(override)) {
    const observation = JSON.parse(readFileSync(override, "utf8"));
    if (observation.refusal) return refuse(observation.refusal);
    return { root, ...observation };
  }
  const entries = [];
  const paths = git("-c", "status.showUntrackedFiles=all", "-c", "diff.ignoreSubmodules=none",
    "status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignore-submodules=none").split("\0");
  for (let index = 0; paths[index]; index += 1) {
    const item = paths[index];
    const status = item.slice(0, 2);
    const path = item.slice(3);
    entries.push({ status, path, ...(/[RC]/u.test(status) ? { origin: paths[++index] } : {}) });
  }
  // These are opaque fixture labels. Cases that need distinct identities for
  // the same status shape provide explicit envelopes at their boundaries.
  const git_delta = { sha256: entries.length.toString(16).padStart(64, "0"), dirty: entries.length > 0, gitlinks: [] };
  return { version: 1, status: git_delta.dirty ? "not_clean" : "clean", root,
    source: `git-commit:${commit}`, tree, entries, refusal: null, source_id: { commit, tree, git_delta } };
}

const [command, rootOption, requestedRoot, jsonOption, ...extra] = process.argv.slice(1);
if (command === resolve("source") && rootOption === "--root" && requestedRoot
    && jsonOption === "--json" && extra.length === 0) {
  const observation = observeFixture(realpathSync.native(requestedRoot));
  writeFileSync(1, JSON.stringify(observation));
  process.exit(observation.refusal ? 1 : 0);
}
