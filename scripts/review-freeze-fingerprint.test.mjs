import assert from "node:assert/strict";
import {
  appendFileSync,
  chmodSync,
  mkdtempSync,
  mkdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { devNull, tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { hashUntrackedFiles } from "./review-freeze-fingerprint.mjs";

const helper = fileURLToPath(
  new URL("./review-freeze-fingerprint.mjs", import.meta.url),
);

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
  env.GIT_CONFIG_GLOBAL = devNull;
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
  const root = mkdtempSync(join(tmpdir(), "termal-freeze-fingerprint-"));
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

test("fingerprint is path-safe and excludes tracked and untracked .beads state", async () => {
  const root = createRepository();
  try {
    mkdirSync(join(root, ".beads"));
    writeFileSync(join(root, ".beads", "tracked state.bin"), "tracked-base\n");
    run("git", ["add", "-f", ".beads/tracked state.bin"], root);
    run(
      "git",
      ["commit", "--quiet", "--no-gpg-sign", "-m", "tracked beads fixture"],
      root,
    );

    writeFileSync(join(root, "untracked file.txt"), "one\n");
    writeFileSync(join(root, ".beads", "ignored state.bin"), "first\n");
    const first = await fingerprint(root);

    writeFileSync(join(root, ".beads", "tracked state.bin"), "tracked-worktree\n");
    const afterTrackedBeadsWorktree = await fingerprint(root);
    assert.deepEqual(afterTrackedBeadsWorktree, first);

    run("git", ["add", "-f", ".beads/tracked state.bin"], root);
    const afterTrackedBeadsIndex = await fingerprint(root);
    assert.deepEqual(afterTrackedBeadsIndex, first);

    writeFileSync(join(root, ".beads", "ignored state.bin"), "second\n");
    const afterBeadsOnly = await fingerprint(root);
    assert.deepEqual(afterBeadsOnly, first);

    writeFileSync(join(root, "untracked file.txt"), "two\n");
    const afterContent = await fingerprint(root);
    assert.notEqual(
      afterContent.untrackedContentSha256,
      first.untrackedContentSha256,
    );

    writeFileSync(join(root, "tracked.txt"), "non-beads tracked change\n");
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
  const hostileRoot = mkdtempSync(join(tmpdir(), "termal-hostile-git-config-"));
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
