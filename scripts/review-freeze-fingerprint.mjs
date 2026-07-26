#!/usr/bin/env node

import { createHash } from "node:crypto";
import {
  closeSync,
  lstatSync,
  openSync,
  readSync,
  readlinkSync,
} from "node:fs";
import { spawn } from "node:child_process";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const FILE_HASH_BUFFER_BYTES = 64 * 1024;
const CAPTURED_GIT_OUTPUT_LIMIT_BYTES = 64 * 1024;
const GIT_DIAGNOSTIC_LIMIT_BYTES = 64 * 1024;

function runGit(args, cwd, onStdout) {
  return new Promise((resolve, reject) => {
    const child = spawn("git", args, {
      cwd,
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
      if (status !== 0) {
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

async function captureGit(args, cwd) {
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
  });
  return Buffer.concat(chunks);
}

async function hashGitOutput(args, cwd) {
  const hash = createHash("sha256");
  await runGit(args, cwd, (chunk) => hash.update(chunk));
  return hash.digest("hex");
}

function writeLength(hash, length) {
  const encoded = Buffer.alloc(8);
  encoded.writeBigUInt64BE(BigInt(length));
  hash.update(encoded);
}

async function nulSeparatedGitValues(args, cwd) {
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
  });
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
    before.ctimeNs === after.ctimeNs
  );
}

function lstatStablePath(path, gitPath) {
  try {
    return lstatSync(path, { bigint: true });
  } catch (error) {
    throw worktreeChangedError(gitPath, error);
  }
}

function assertFilesystemEntryStable(path, gitPath, before) {
  const after = lstatStablePath(path, gitPath);
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
  assertFilesystemEntryStable(path, gitPath, metadata);
}

export async function hashUntrackedFiles(root, hooks = {}) {
  const paths = await nulSeparatedGitValues(
    [
      "ls-files",
      "--others",
      "--exclude-standard",
      "-z",
      "--",
      ":(exclude).beads",
    ],
    root,
  );
  const hash = createHash("sha256");
  for (const gitPath of paths) {
    const path = filesystemPath(root, gitPath);
    hooks.beforeEntry?.(gitPath);
    const metadata = lstatStablePath(path, gitPath);
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
      assertFilesystemEntryStable(path, gitPath, metadata);
    }
  }
  return hash.digest("hex");
}

export async function main() {
  const requestedRoot = process.argv[2] ?? process.cwd();
  const root = (await captureGit(["rev-parse", "--show-toplevel"], requestedRoot))
    .toString("utf8")
    .trim();
  const headCommit = (
    await captureGit(["rev-parse", "--verify", "HEAD^{commit}"], root)
  )
    .toString("utf8")
    .trim();
  const trackedIndexDiffSha256 = await hashGitOutput(
    ["diff", "--cached", "HEAD", "--binary", "--", ":(exclude).beads"],
    root,
  );
  const trackedHeadDiffSha256 = await hashGitOutput(
    ["diff", "HEAD", "--binary", "--", ":(exclude).beads"],
    root,
  );
  const statusSha256 = await hashGitOutput(
    [
      "status",
      "--short",
      "-z",
      "--untracked-files=all",
      "--",
      ":(exclude).beads",
    ],
    root,
  );
  const untrackedContentSha256 = await hashUntrackedFiles(root);

  process.stdout.write(
    [
      `headCommit=${headCommit}`,
      `trackedIndexDiffSha256=${trackedIndexDiffSha256}`,
      `trackedHeadDiffSha256=${trackedHeadDiffSha256}`,
      `statusSha256=${statusSha256}`,
      `untrackedContentSha256=${untrackedContentSha256}`,
      "",
    ].join("\n"),
  );
}

if (
  process.argv[1] &&
  pathToFileURL(process.argv[1]).href === import.meta.url
) {
  try {
    await main();
  } catch (error) {
    process.stderr.write(
      `[review-freeze-fingerprint] ${error instanceof Error ? error.message : String(error)}\n`,
    );
    process.exitCode = 1;
  }
}
