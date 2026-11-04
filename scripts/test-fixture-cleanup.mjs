// Fixture directory cleanup for the helper tests. A passing test's fixture
// directory is removed; a failed or hung test keeps it, with every log its
// worker processes wrote, and names it on stderr, so a slow worker and a hung
// one can be told apart afterwards. A hung test is one the runner's liveness
// guard (HELPER_TEST_HANG_GUARD_MS in scripts/test-launcher.mjs) stopped.
// The verdict is the test's own, read in its after-hook: register it on a test
// whose outcome is decided in its own body, not on a parent whose subtests
// decide it, since a subtest's failure reaches the parent only after the hook.
// On Windows another process can briefly hold a file a test has just written,
// which refuses its removal. rmSync's own retries don't wait for that: on the
// pinned Node they fail at once. So removal waits here, within a budget that
// only detects a holder that never lets go, and then reports the first error.
import { rmSync } from "node:fs";

const REMOVE_BUDGET_MS = 10_000;
const REMOVE_BACKOFF_MS = 50;
const REMOVE_SHARING_ERRORS = new Set(["EPERM", "EACCES", "EBUSY", "ENOTEMPTY"]);

const sleepSync = (milliseconds) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, milliseconds);

export function removeFixture(path, {
  platform = process.platform,
  remove = rmSync,
  now = () => performance.now(),
  sleep = sleepSync,
  budgetMs = REMOVE_BUDGET_MS,
} = {}) {
  const started = now();
  let firstError;
  for (;;) {
    try {
      remove(path, { recursive: true, force: true });
      return;
    } catch (error) {
      if (platform !== "win32" || !REMOVE_SHARING_ERRORS.has(error.code)) throw error;
      firstError ??= error;
      if (now() - started >= budgetMs) throw firstError;
      sleep(REMOVE_BACKOFF_MS);
    }
  }
}

export function removeFixtureOnPass(t, path) {
  t.after(() => {
    if (t.passed) {
      removeFixture(path);
      return;
    }
    const verdict = t.error?.failureType === "testTimeoutFailure" ? "hung" : "failed";
    process.stderr.write(`${verdict}: ${t.name}; fixture kept at ${path}\n`);
  });
}
