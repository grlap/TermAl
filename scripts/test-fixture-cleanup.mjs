// Fixture directory cleanup for the helper tests. A passing test's fixture
// directory is removed; a failed or hung test keeps it, with every log its
// worker processes wrote, and names it on stderr, so a slow worker and a hung
// one can be told apart afterwards. A hung test is one the runner's liveness
// guard (HELPER_TEST_HANG_GUARD_MS in scripts/test-launcher.mjs) stopped.
// The verdict is the test's own, read in its after-hook: register it on a test
// whose outcome is decided in its own body, not on a parent whose subtests
// decide it, since a subtest's failure reaches the parent only after the hook.
import { rmSync } from "node:fs";

export function removeFixtureOnPass(t, path) {
  t.after(() => {
    if (t.passed) {
      rmSync(path, { recursive: true, force: true });
      return;
    }
    const verdict = t.error?.failureType === "testTimeoutFailure" ? "hung" : "failed";
    process.stderr.write(`${verdict}: ${t.name}; fixture kept at ${path}\n`);
  });
}
