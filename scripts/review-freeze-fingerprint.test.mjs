import assert from "node:assert/strict";
import {
  appendFileSync,
  chmodSync,
  lstatSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import { devNull } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  compositeFingerprint,
  hashUntrackedFiles,
} from "./review-freeze-fingerprint.mjs";
import { testTempDirectory } from "./test-temp-root.mjs";

const helper = fileURLToPath(
  new URL("./review-freeze-fingerprint.mjs", import.meta.url),
);

test("composite fingerprint uses the pinned ordered component contract", () => {
  const components = {
    root: "C:/repo",
    headCommit: "1111",
    trackedIndexDiffSha256: "2222",
    trackedHeadDiffSha256: "3333",
    statusSha256: "4444",
    untrackedContentSha256: "5555",
  };
  assert.equal(
    compositeFingerprint({ ...components, ignored: "not part of the contract" }),
    "6a63123131b929728cf3cc8a88cdde5d33bd25f1f106259e92d067c9cd14a5b0",
  );
});

test("fingerprint CLI keeps its component-only wire format", async () => {
  const root = createRepository();
  try {
    assert.deepEqual(Object.keys(await fingerprint(root)), [
      "headCommit",
      "trackedIndexDiffSha256",
      "trackedHeadDiffSha256",
      "statusSha256",
      "untrackedContentSha256",
    ]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

function run(command, args, cwd) {
  const env = { ...process.env };
  for (const key of Object.keys(env)) {
    if (
      key === "GIT_CONFIG_COUNT" ||
      key.startsWith("GIT_CONFIG_KEY_") ||
      key.startsWith("GIT_CONFIG_VALUE_")
    ) {
      delete env[key];
    }
  }
  env.GIT_CONFIG_NOSYSTEM = "1";
  // Git for Windows rejects Node's `\\.\nul` device path when it is used as
  // GIT_CONFIG_GLOBAL, while the native DOS device name is accepted.
  env.GIT_CONFIG_GLOBAL = process.platform === "win32" ? "NUL" : devNull;
  env.GIT_TERMINAL_PROMPT = "0";
  const result = spawnSync(command, args, {
    cwd,
    encoding: "utf8",
    env,
    windowsHide: true,
  });
  assert.equal(
    result.status,
    0,
    `${command} ${args.join(" ")} failed: ${result.stderr}`,
  );
  return result.stdout;
}

function createRepository() {
  const root = mkdtempSync(join(testTempDirectory(), "termal-freeze-fingerprint-"));
  run("git", ["init", "--quiet", "--template="], root);
  run("git", ["config", "user.email", "termal-test@example.invalid"], root);
  run("git", ["config", "user.name", "TermAl Test"], root);
  writeFileSync(join(root, "tracked.txt"), "base\n");
  run("git", ["add", "tracked.txt"], root);
  run("git", ["commit", "--quiet", "--no-gpg-sign", "-m", "base"], root);
  return root;
}

async function fingerprint(root) {
  const output = run(process.execPath, [helper, root], root);
  return Object.fromEntries(
    output
      .trim()
      .split("\n")
      .map((line) => line.split("=")),
  );
}

test("staged-content fingerprint is identical in LF, CRLF and no-checkout worktrees", async () => {
  const root = createRepository();
  const crlfRoot = `${root}-crlf`;
  const emptyRoot = `${root}-no-checkout`;
  const worktrees = [];
  try {
    const base = run("git", ["rev-parse", "HEAD"], root).trim();
    writeFileSync(join(root, "tracked.txt"), "staged-one\nsecond line\n");
    run("git", ["add", "tracked.txt"], root);
    run("git", ["commit", "--quiet", "--no-gpg-sign", "-m", "staged content"], root);
    const staged = run("git", ["rev-parse", "HEAD"], root).trim();

    run("git", ["-c", "core.autocrlf=true", "worktree", "add", "--detach", crlfRoot, staged], root);
    worktrees.push(crlfRoot);
    run("git", ["worktree", "add", "--detach", "--no-checkout", emptyRoot, base], root);
    worktrees.push(emptyRoot);
    run("git", ["read-tree", staged], emptyRoot);
    // Keep the checkout's stat-clean index while giving every index the same base.
    run("git", ["update-ref", "HEAD", base], crlfRoot);
    run("git", ["reset", "--soft", base], root);

    // Non-racy stat-clean entries let Git reuse checkout bytes for cached diffs.
    // Make this precondition deterministic without sleeping for timestamp ticks.
    const older = new Date(Date.now() - 10_000);
    for (const [checkout, autocrlf] of [[root, "false"], [crlfRoot, "true"]]) {
      const trackedPath = join(checkout, "tracked.txt");
      utimesSync(trackedPath, older, older);
      run("git", ["-c", `core.autocrlf=${autocrlf}`, "update-index", "--refresh"], checkout);
      const indexPath = run("git", ["rev-parse", "--git-path", "index"], checkout).trim();
      assert.ok(statSync(trackedPath).mtimeMs < statSync(resolve(checkout, indexPath)).mtimeMs);
      assert.equal(run("git", ["-c", `core.autocrlf=${autocrlf}`, "diff-files", "--raw"], checkout), "");
    }

    assert.equal(readFileSync(join(root, "tracked.txt"), "utf8"), "staged-one\nsecond line\n");
    assert.equal(readFileSync(join(crlfRoot, "tracked.txt"), "utf8"), "staged-one\r\nsecond line\r\n");
    const index = run("git", ["ls-files", "--stage", "-z"], root);
    assert.equal(run("git", ["ls-files", "--stage", "-z"], crlfRoot), index);
    assert.equal(run("git", ["ls-files", "--stage", "-z"], emptyRoot), index);

    const lf = await fingerprint(root);
    const crlf = await fingerprint(crlfRoot);
    const noCheckout = await fingerprint(emptyRoot);
    assert.equal(crlf.trackedIndexDiffSha256, lf.trackedIndexDiffSha256);
    assert.equal(noCheckout.trackedIndexDiffSha256, lf.trackedIndexDiffSha256);
    // The HEAD diff intentionally records checkout state, including its EOLs
    // or absent tracked files, rather than just the shared staged blobs.
    assert.notEqual(crlf.trackedHeadDiffSha256, lf.trackedHeadDiffSha256);
    assert.notEqual(noCheckout.trackedHeadDiffSha256, lf.trackedHeadDiffSha256);

    writeFileSync(join(crlfRoot, "tracked.txt"), "staged-two\r\nsecond line\r\n");
    run("git", ["-c", "core.autocrlf=true", "add", "tracked.txt"], crlfRoot);
    const changed = await fingerprint(crlfRoot);
    assert.notEqual(changed.trackedIndexDiffSha256, lf.trackedIndexDiffSha256);
    assert.notEqual(changed.trackedHeadDiffSha256, crlf.trackedHeadDiffSha256);
  } finally {
    for (const worktree of worktrees.reverse()) {
      run("git", ["worktree", "remove", "--force", worktree], root);
    }
    rmSync(root, { recursive: true, force: true });
  }
});

test("fingerprint detects staged content changes even when short status is unchanged", async () => {
  const root = createRepository();
  try {
    writeFileSync(join(root, "tracked.txt"), "staged-one\n");
    run("git", ["add", "tracked.txt"], root);
    const first = await fingerprint(root);

    writeFileSync(join(root, "tracked.txt"), "staged-two\n");
    run("git", ["add", "tracked.txt"], root);
    const second = await fingerprint(root);

    assert.notEqual(first.trackedHeadDiffSha256, second.trackedHeadDiffSha256);
    assert.notEqual(first.trackedIndexDiffSha256, second.trackedIndexDiffSha256);
    assert.equal(first.statusSha256, second.statusSha256);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("fingerprint detects index-only drift while worktree and status stay fixed", async () => {
  const root = createRepository();
  try {
    writeFileSync(join(root, "tracked.txt"), "staged-one\n");
    run("git", ["add", "tracked.txt"], root);
    writeFileSync(join(root, "tracked.txt"), "worktree\n");
    const first = await fingerprint(root);

    writeFileSync(join(root, "tracked.txt"), "staged-two\n");
    run("git", ["add", "tracked.txt"], root);
    writeFileSync(join(root, "tracked.txt"), "worktree\n");
    const second = await fingerprint(root);

    assert.equal(first.trackedHeadDiffSha256, second.trackedHeadDiffSha256);
    assert.equal(first.statusSha256, second.statusSha256);
    assert.notEqual(first.trackedIndexDiffSha256, second.trackedIndexDiffSha256);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("fingerprint includes the exact HEAD commit", async () => {
  const root = createRepository();
  try {
    const first = await fingerprint(root);
    run(
      "git",
      ["commit", "--quiet", "--no-gpg-sign", "--allow-empty", "-m", "next"],
      root,
    );
    const second = await fingerprint(root);

    assert.notEqual(first.headCommit, second.headCommit);
    assert.equal(first.trackedIndexDiffSha256, second.trackedIndexDiffSha256);
    assert.equal(first.trackedHeadDiffSha256, second.trackedHeadDiffSha256);
    assert.equal(first.statusSha256, second.statusSha256);
    assert.equal(first.untrackedContentSha256, second.untrackedContentSha256);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("fingerprint is path-safe for file names with spaces", async () => {
  const root = createRepository();
  try {
    writeFileSync(join(root, "untracked file.txt"), "one\n");
    const first = await fingerprint(root);
    assert.deepEqual(await fingerprint(root), first);

    writeFileSync(join(root, "untracked file.txt"), "two\n");
    const afterContent = await fingerprint(root);
    assert.notEqual(
      afterContent.untrackedContentSha256,
      first.untrackedContentSha256,
    );

    writeFileSync(join(root, "tracked.txt"), "tracked change\n");
    const afterTrackedContent = await fingerprint(root);
    assert.notEqual(
      afterTrackedContent.trackedHeadDiffSha256,
      afterContent.trackedHeadDiffSha256,
    );
    assert.notEqual(afterTrackedContent.statusSha256, afterContent.statusSha256);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("fingerprint streams multi-chunk untracked files", async () => {
  const root = createRepository();
  try {
    const path = join(root, "large-untracked.bin");
    writeFileSync(path, Buffer.alloc(192 * 1024, 0x61));
    const first = await fingerprint(root);

    writeFileSync(path, Buffer.alloc(192 * 1024, 0x62));
    const second = await fingerprint(root);
    assert.notEqual(
      second.untrackedContentSha256,
      first.untrackedContentSha256,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test(
  "fingerprint handles tracked Git output larger than the child-process default buffer",
  async () => {
    const root = createRepository();
    try {
      writeFileSync(
        join(root, "tracked.txt"),
        "large changed line\n".repeat(90_000),
      );
      const result = await fingerprint(root);
      assert.match(result.trackedHeadDiffSha256, /^[0-9a-f]{64}$/);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test(
  "untracked executable semantics affect the fingerprint on Unix",
  { skip: process.platform === "win32" },
  async () => {
    const root = createRepository();
    try {
      const path = join(root, "run-me.sh");
      writeFileSync(path, "#!/bin/sh\nexit 0\n");
      chmodSync(path, 0o644);
      const first = await fingerprint(root);

      chmodSync(path, 0o755);
      const second = await fingerprint(root);
      assert.notEqual(
        first.untrackedContentSha256,
        second.untrackedContentSha256,
      );
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test("test repositories ignore hostile inherited Git configuration", async () => {
  const hostileRoot = mkdtempSync(join(testTempDirectory(), "termal-hostile-git-config-"));
  const hostileConfig = join(hostileRoot, "gitconfig");
  const previousGlobalConfig = process.env.GIT_CONFIG_GLOBAL;
  try {
    writeFileSync(
      hostileConfig,
      "[commit]\n\tgpgSign = true\n[init]\n\tdefaultBranch = hostile\n",
    );
    process.env.GIT_CONFIG_GLOBAL = hostileConfig;
    const root = createRepository();
    try {
      const result = await fingerprint(root);
      assert.match(result.headCommit, /^[0-9a-f]{40,64}$/);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  } finally {
    if (previousGlobalConfig === undefined) {
      delete process.env.GIT_CONFIG_GLOBAL;
    } else {
      process.env.GIT_CONFIG_GLOBAL = previousGlobalConfig;
    }
    rmSync(hostileRoot, { recursive: true, force: true });
  }
});

test("fingerprint reports an untracked file removed after enumeration", async () => {
  const root = createRepository();
  try {
    const path = join(root, "removed-during-freeze.txt");
    writeFileSync(path, "present\n");
    await assert.rejects(
      () =>
        hashUntrackedFiles(root, {
          beforeEntry() {
            rmSync(path);
          },
        }),
      /worktree changed while fingerprinting .*; rerun/,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("fingerprint rejects an untracked file mutated during hashing", async () => {
  const root = createRepository();
  try {
    const path = join(root, "mutated-during-freeze.txt");
    writeFileSync(path, "before\n");
    await assert.rejects(
      () =>
        hashUntrackedFiles(root, {
          afterOpen() {
            appendFileSync(path, "after\n");
          },
        }),
      /worktree changed while fingerprinting .*; rerun/,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("fingerprint tolerates ctime-only drift during hashing only on Windows", async () => {
  const root = createRepository();
  try {
    writeFileSync(join(root, "metadata only.txt"), "unchanged\n");
    const expected = await hashUntrackedFiles(root);
    let opened = false;
    const observedCtimes = [];
    const capture = () => hashUntrackedFiles(root, {
      afterOpen() { opened = true; },
      lstat(path, options) {
        const metadata = lstatSync(path, options);
        metadata.ctimeNs = opened ? 2n : 1n;
        observedCtimes.push(metadata.ctimeNs);
        return metadata;
      },
    });
    if (process.platform === "win32") {
      assert.equal(await capture(), expected);
    } else {
      await assert.rejects(capture, /worktree changed while fingerprinting .*; rerun/);
    }
    assert.deepEqual(observedCtimes, [1n, 2n]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

for (const field of ["dev", "ino", "mode", "size", "mtimeNs"]) {
  test(`fingerprint rejects ${field} drift even with a ctime-only exception`, async () => {
    const root = createRepository();
    try {
      writeFileSync(join(root, "metadata-drift.txt"), "unchanged\n");
      let opened = false;
      await assert.rejects(() => hashUntrackedFiles(root, {
        afterOpen() { opened = true; },
        lstat(path, options) {
          const metadata = lstatSync(path, options);
          metadata.ctimeNs = opened ? 2n : 1n;
          if (opened) metadata[field] += 1n;
          return metadata;
        },
      }), /worktree changed while fingerprinting .*; rerun/);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}

test("fingerprint rejects an mtime change without a content or size change", async () => {
  const root = createRepository();
  try {
    const path = join(root, "mtime-drift.txt");
    writeFileSync(path, "unchanged\n");
    const earlier = new Date(Date.now() - 60_000);
    await assert.rejects(() => hashUntrackedFiles(root, {
      afterOpen() { utimesSync(path, earlier, earlier); },
    }), /worktree changed while fingerprinting .*; rerun/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
