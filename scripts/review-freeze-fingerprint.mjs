#!/usr/bin/env node

import { createHash } from "node:crypto";
import {
  closeSync,
  lstatSync,
  openSync,
  readSync,
  readlinkSync,
  realpathSync,
} from "node:fs";
import { spawn } from "node:child_process";
import { devNull } from "node:os";
import { isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const fingerprintComponentNames = Object.freeze([
  "root",
  "headCommit",
  "trackedIndexDiffSha256",
  "trackedHeadDiffSha256",
  "statusSha256",
  "untrackedContentSha256",
]);

export function compositeFingerprint(source) {
  const components = Object.fromEntries(
    fingerprintComponentNames.map((name) => [name, source[name]]),
  );
  return createHash("sha256").update(JSON.stringify(components)).digest("hex");
}

const FILE_HASH_BUFFER_BYTES = 64 * 1024;
const CAPTURED_GIT_OUTPUT_LIMIT_BYTES = 64 * 1024;
const GIT_DIAGNOSTIC_LIMIT_BYTES = 64 * 1024;

export function isolatedGitEnvironment(source = process.env) {
  const env = { ...source };
  for (const key of Object.keys(env)) {
    if (key.toUpperCase().startsWith("GIT_")) delete env[key];
  }
  env.GIT_CONFIG_NOSYSTEM = "1";
  env.GIT_CONFIG_GLOBAL = process.platform === "win32" ? "NUL" : devNull;
  env.GIT_ATTR_NOSYSTEM = "1";
  env.GIT_OPTIONAL_LOCKS = "0";
  env.GIT_TERMINAL_PROMPT = "0";
  return env;
}

// `allowAbsent` accepts exit status 1 as an empty answer, as `git config
// --get-regexp` reports no match.
function runGit(args, cwd, onStdout, env, allowAbsent = false) {
  return new Promise((resolve, reject) => {
    const child = spawn("git", args, {
      cwd,
      env,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    const stderrChunks = [];
    let stderrBytes = 0;
    let handlerError;

    child.stdout.on("data", (chunk) => {
      if (handlerError) {
        return;
      }
      try {
        onStdout(chunk);
      } catch (error) {
        handlerError = error;
        child.kill();
      }
    });
    child.stderr.on("data", (chunk) => {
      if (stderrBytes >= GIT_DIAGNOSTIC_LIMIT_BYTES) {
        return;
      }
      const remaining = GIT_DIAGNOSTIC_LIMIT_BYTES - stderrBytes;
      const captured = chunk.subarray(0, remaining);
      stderrChunks.push(captured);
      stderrBytes += captured.length;
    });
    child.on("error", reject);
    child.on("close", (status, signal) => {
      if (handlerError) {
        reject(handlerError);
        return;
      }
      if (status !== 0 && !(allowAbsent && status === 1)) {
        const stderr = Buffer.concat(stderrChunks).toString("utf8").trim();
        const outcome = signal ? `signal ${signal}` : `exit ${status}`;
        reject(
          new Error(
            `git ${args.join(" ")} failed: ${stderr || outcome}`,
          ),
        );
        return;
      }
      resolve();
    });
  });
}

async function captureGit(args, cwd, env, allowAbsent = false) {
  const chunks = [];
  let totalBytes = 0;
  await runGit(args, cwd, (chunk) => {
    totalBytes += chunk.length;
    if (totalBytes > CAPTURED_GIT_OUTPUT_LIMIT_BYTES) {
      throw new Error(
        `git ${args.join(" ")} exceeded the ${CAPTURED_GIT_OUTPUT_LIMIT_BYTES}-byte capture limit`,
      );
    }
    chunks.push(chunk);
  }, env, allowAbsent);
  return Buffer.concat(chunks);
}

async function hashGitOutput(args, cwd, env) {
  const hash = createHash("sha256");
  await runGit(args, cwd, (chunk) => hash.update(chunk), env);
  return hash.digest("hex");
}

function writeLength(hash, length) {
  const encoded = Buffer.alloc(8);
  encoded.writeBigUInt64BE(BigInt(length));
  hash.update(encoded);
}

async function nulSeparatedGitValues(args, cwd, env) {
  const values = [];
  let pending = Buffer.alloc(0);
  await runGit(args, cwd, (chunk) => {
    const buffer =
      pending.length === 0 ? chunk : Buffer.concat([pending, chunk]);
    let start = 0;
    while (start < buffer.length) {
      const end = buffer.indexOf(0, start);
      if (end === -1) {
        break;
      }
      if (end > start) {
        values.push(Buffer.from(buffer.subarray(start, end)));
      }
      start = end + 1;
    }
    pending = Buffer.from(buffer.subarray(start));
  }, env);
  if (pending.length !== 0) {
    throw new Error("git returned a non-NUL-terminated path list");
  }
  return values;
}

function filesystemPath(root, gitPath) {
  return Buffer.concat([
    Buffer.from(join(root, ".")),
    Buffer.from("/"),
    gitPath,
  ]);
}

function describeGitPath(gitPath) {
  return gitPath.toString("utf8");
}

function worktreeChangedError(gitPath, error) {
  const detail =
    error instanceof Error && error.message ? ` (${error.message})` : "";
  return new Error(
    `worktree changed while fingerprinting \`${describeGitPath(gitPath)}\`; rerun${detail}`,
  );
}

function sameFilesystemEntry(before, after) {
  return (
    before.dev === after.dev &&
    before.ino === after.ino &&
    before.mode === after.mode &&
    before.size === after.size &&
    before.mtimeNs === after.mtimeNs &&
    // Windows ChangeTime can advance on AV/indexer metadata writes without a
    // content change; retain every other identity, size and mtime check.
    (process.platform === "win32" || before.ctimeNs === after.ctimeNs)
  );
}

function lstatStablePath(path, gitPath, hooks) {
  try {
    return (hooks.lstat ?? lstatSync)(path, { bigint: true });
  } catch (error) {
    throw worktreeChangedError(gitPath, error);
  }
}

function assertFilesystemEntryStable(path, gitPath, before, hooks) {
  const after = lstatStablePath(path, gitPath, hooks);
  if (!sameFilesystemEntry(before, after)) {
    throw worktreeChangedError(gitPath);
  }
}

function hashRegularFile(hash, path, gitPath, metadata, hooks) {
  // Git for Windows normally records new worktree files as non-executable
  // because filesystem execute bits are not meaningful there. On Unix, Git
  // distinguishes only executable from non-executable regular files.
  const executable =
    process.platform !== "win32" && (metadata.mode & 0o111n) !== 0n ? 1 : 0;
  hash.update(Buffer.from([executable]));
  writeLength(hash, metadata.size);
  let descriptor;
  let totalBytes = 0n;
  try {
    descriptor = openSync(path, "r");
    hooks.afterOpen?.(gitPath);
    const buffer = Buffer.allocUnsafe(FILE_HASH_BUFFER_BYTES);
    while (true) {
      const bytesRead = readSync(descriptor, buffer, 0, buffer.length, null);
      if (bytesRead === 0) {
        break;
      }
      totalBytes += BigInt(bytesRead);
      hash.update(buffer.subarray(0, bytesRead));
    }
  } catch (error) {
    throw worktreeChangedError(gitPath, error);
  } finally {
    if (descriptor !== undefined) {
      closeSync(descriptor);
    }
  }
  if (totalBytes !== metadata.size) {
    throw worktreeChangedError(gitPath);
  }
  assertFilesystemEntryStable(path, gitPath, metadata, hooks);
}

export async function hashUntrackedFiles(root, hooks = {}, env = isolatedGitEnvironment()) {
  const paths = await nulSeparatedGitValues(
    [
      "ls-files",
      "--others",
      "--exclude-standard",
      "-z",
    ],
    root,
    env,
  );
  const hash = createHash("sha256");
  for (const gitPath of paths) {
    const path = filesystemPath(root, gitPath);
    hooks.beforeEntry?.(gitPath);
    const metadata = lstatStablePath(path, gitPath, hooks);
    const kind = metadata.isSymbolicLink() ? "symlink" : "file";
    if (kind === "file" && !metadata.isFile()) {
      throw new Error(
        `unsupported untracked filesystem entry: ${describeGitPath(gitPath)}`,
      );
    }
    const kindBytes = Buffer.from(kind);
    writeLength(hash, kindBytes.length);
    hash.update(kindBytes);
    writeLength(hash, gitPath.length);
    hash.update(gitPath);
    if (kind === "file") {
      hashRegularFile(hash, path, gitPath, metadata, hooks);
    } else {
      let content;
      try {
        content = readlinkSync(path, { encoding: "buffer" });
      } catch (error) {
        throw worktreeChangedError(gitPath, error);
      }
      writeLength(hash, content.length);
      hash.update(content);
      assertFilesystemEntryStable(path, gitPath, metadata, hooks);
    }
  }
  return hash.digest("hex");
}

export async function captureFingerprint(requestedRoot = process.cwd(), sourceEnv = process.env) {
  const env = isolatedGitEnvironment(sourceEnv);
  const root = (await captureGit(["rev-parse", "--show-toplevel"], requestedRoot, env))
    .toString("utf8")
    .trim();
  const headCommit = (
    await captureGit(["rev-parse", "--verify", "HEAD^{commit}"], root, env)
  )
    .toString("utf8")
    .trim();
  const trackedIndexDiffSha256 = await hashGitOutput(
    // Hash index identities directly: cached patch rendering can reuse stat-clean
    // checkout files, whose line endings need not match their staged blobs.
    ["ls-files", "--stage", "-z"],
    root,
    env,
  );
  const trackedHeadDiffSha256 = await hashGitOutput(
    ["diff", "HEAD", "--binary"],
    root,
    env,
  );
  const statusSha256 = await hashGitOutput(
    [
      "status",
      "--short",
      "-z",
      "--untracked-files=all",
    ],
    root,
    env,
  );
  const untrackedContentSha256 = await hashUntrackedFiles(root, {}, env);

  const fields = {
    root,
    headCommit,
    trackedIndexDiffSha256,
    trackedHeadDiffSha256,
    statusSha256,
    untrackedContentSha256,
  };
  const fingerprint = compositeFingerprint(fields);
  return { ...fields, fingerprint };
}

// Commit admission uses recursive submodule status, root/child filter config,
// one root porcelain dirt check and a root HEAD reread. No full file or index
// listing is derived. Clean nested pinned children are supported. Git status
// can miss edits to assume-unchanged/skip-worktree files; that is a documented
// limitation. Empty skipped/gitlinks fields retain the existing record shape.
// Child filters are checked before root status or execution-boundary fingerprints.
export async function captureCommitInputStatus(requestedRoot = process.cwd(), sourceEnv = process.env) {
  const env = isolatedGitEnvironment(sourceEnv);
  const git = commitInputGitArgs;
  const root = (await captureGit(git(["rev-parse", "--show-toplevel"]), requestedRoot, env))
    .toString("utf8")
    .trim();
  const head = async () => (
    await captureGit(git(["rev-parse", "--verify", "HEAD^{commit}"]), root, env)
  )
    .toString("utf8")
    .trim();
  const filters = (await captureGit(
    git(["config", "--null", "--get-regexp", "^filter\\..*\\.(clean|smudge|process)$"]),
    root,
    env,
    true,
  ))
    .toString("utf8")
    .split("\0")
    .filter((entry) => entry.length > 0)
    .map((entry) => entry.split("\n", 1)[0]);
  const commit = await head();
  if (!/^[0-9a-f]{40}$/u.test(commit)) throw new Error("HEAD is not a full SHA-1 commit id (unsupported object format)");
  if (realpathSync.native(root) !== realpathSync.native(requestedRoot)) throw new Error("committed-input cwd must be the worktree root");
  if (filters.length) {
    return { root, commit, entries: [], skipped: [], filters, gitlinks: [] };
  }
  const skipped = [];
  {
    let pins;
    try {
      pins = (await captureGit(git(["submodule", "status", "--recursive"]), root, env)).toString("utf8");
    } catch (error) {
      throw new Error(`submodule initialization/pin or .gitmodules mapping check failed: ${error.message}`, { cause: error });
    }
    const rows = pins.split("\n").filter(Boolean);
    if (rows.some((row) => row[0] !== " ")) {
      throw new Error("submodule is not initialized at its pin (git submodule status --recursive)");
    }
    const canonicalRoot = realpathSync.native(root);
    for (const row of rows) {
      if (!/^ [0-9a-f]{40} .+/u.test(row)) throw new Error("malformed git submodule status row");
      // Git appends an optional ` (describe)` suffix after the path.
      const tail = row.slice(42);
      const suffix = tail.endsWith(")") ? tail.lastIndexOf(" (") : -1;
      const path = suffix === -1 ? tail : tail.slice(0, suffix);
      let child;
      // Git's successful clean-prefix row supplies initialization and pin
      // judgment. Keep filter-config paths within this root.
      try {
        child = realpathSync.native(join(root, path));
      } catch {
        throw new Error(`submodule ${path} is not initialized`);
      }
      const fromRoot = relative(canonicalRoot, child);
      if (!fromRoot || isAbsolute(fromRoot) || fromRoot.split(/[\\/]/u).includes("..")) {
        throw new Error(`submodule ${path} is outside its worktree root`);
      }
      const childFilters = (await captureGit(
        git(["config", "--null", "--get-regexp", "^filter\\..*\\.(clean|smudge|process)$"]), child, env, true,
      )).toString("utf8").split("\0").filter(Boolean).map((record) => record.split("\n", 1)[0]);
      filters.push(...childFilters.map((key) => `${path}: ${key}`));
    }
    if (filters.length || skipped.length) return { root, commit, entries: [], skipped, filters, gitlinks: [] };
  }
  const records = await nulSeparatedGitValues(
    // Command-line -u is not forwarded to recursive child status. A -c
    // setting is inherited and overrides a child's status.showUntrackedFiles.
    git(["-c", "status.showUntrackedFiles=all", "-c", "diff.ignoreSubmodules=none", "status", "--porcelain=v1", "--untracked-files=all", "--ignore-submodules=none", "-z"]),
    root,
    env,
  );
  const entries = [];
  for (let index = 0; index < records.length; index += 1) {
    const record = records[index].toString("utf8");
    if (record.length < 4 || record[2] !== " ") {
      throw new Error("git status returned a malformed porcelain entry");
    }
    const entry = { status: record.slice(0, 2), path: record.slice(3) };
    if (/[RC]/u.test(entry.status)) {
      index += 1;
      if (index >= records.length) {
        throw new Error("git status returned a rename or copy without its origin path");
      }
      entry.origin = records[index].toString("utf8");
    }
    entries.push(entry);
  }
  if (await head() !== commit) {
    throw new Error("HEAD moved while the committed-input status was captured");
  }
  return { root, commit, entries, skipped, filters, gitlinks: [] };
}

// The arguments of every committed-input Git call, as `ReviewFreezeGit::command`
// in src/review_freeze.rs pins them: no fsmonitor hook or daemon is consulted
// (it could report a changed path as unchanged), and both the excludes and the
// attributes file at the null device (NUL on Windows, /dev/null elsewhere), so
// neither XDG default changes what is clean.
export function commitInputGitArgs(args) {
  const nullDevice = process.platform === "win32" ? "NUL" : devNull;
  return [
    "-c", "core.fsmonitor=false",
    "-c", `core.excludesFile=${nullDevice}`,
    "-c", `core.attributesFile=${nullDevice}`,
    ...args,
  ];
}

export async function main() {
  const actual = await captureFingerprint(process.argv[2] ?? process.cwd());

  process.stdout.write(
    [
      `headCommit=${actual.headCommit}`,
      `trackedIndexDiffSha256=${actual.trackedIndexDiffSha256}`,
      `trackedHeadDiffSha256=${actual.trackedHeadDiffSha256}`,
      `statusSha256=${actual.statusSha256}`,
      `untrackedContentSha256=${actual.untrackedContentSha256}`,
      "",
    ].join("\n"),
  );
}

// Node resolves this module's own path through links, while argv keeps the
// caller's spelling: compare canonical paths, or a run through an alias
// (macOS /var, a Windows junction) would exit silently without output.
function invokedDirectly() {
  if (!process.argv[1]) return false;
  try {
    return realpathSync.native(resolve(process.argv[1])) ===
      realpathSync.native(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

if (invokedDirectly()) {
  try {
    await main();
  } catch (error) {
    process.stderr.write(
      `[review-freeze-fingerprint] ${error instanceof Error ? error.message : String(error)}\n`,
    );
    process.exitCode = 1;
  }
}
