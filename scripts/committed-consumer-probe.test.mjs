// A real short check for the disposable consumer integration fixture. The
// maintained launcher runs this file against the consumer as its actual cwd.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import test from "node:test";

test("the consumer root and initialized direct children remain at the supplied commits", () => {
  const expected = JSON.parse(process.env.TERMAL_TEST_CONSUMER_PIN_PROBE);
  const git = (root, ...args) => execFileSync("git", args, {
    cwd: root, encoding: "utf8", windowsHide: true,
  }).trim();
  assert.equal(git(process.cwd(), "rev-parse", "HEAD"), expected.commit);
  assert.equal(git(process.cwd(), "-c", "status.showUntrackedFiles=all", "status",
    "--porcelain=v1", "--untracked-files=all", "--ignore-submodules=none"), "");
  for (const [path, commit] of Object.entries(expected.children)) {
    assert.equal(git(path, "rev-parse", "HEAD"), commit);
  }
});
