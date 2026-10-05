// Test counts for a focused launcher run.
//
// Owns: reading the wrapped test runner's own result lines from a focused
// stage's log and summing them into { passed, failed, ignored }, and the
// summary lines that report those counts. A focused run executes whatever
// follows `--`, so its exit status alone says nothing about how many tests ran;
// these counts are what lets the TermAl host credit a passing focused run from
// the run's own results.json (docs/test.md, "Which stages count as tests").
// Does not own: running the stage, the run records, or the host's credit rule
// (src/engram_launcher_stages.rs). Recognises cargo's libtest summary line
// only; any other runner records no counts, and a run without counts is never
// credited.

/** ANSI terminal control sequences, as the launcher's own diagnostics strip them. */
// eslint-disable-next-line no-control-regex
const terminalCodes = /\u001b\[[0-9;?]*[A-Za-z]/gu;

/** libtest's per-binary summary: `test result: ok. 3 passed; 0 failed; 1 ignored; …`. */
const cargoResultLine =
  /^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;/u;

/**
 * The counts of every libtest summary line in `text`, summed across test
 * binaries, or `undefined` when `text` has none.
 */
export function focusedTestCounts(text) {
  let counts;
  for (const raw of text.split(/\r?\n/u)) {
    // A forced-colour runner wraps `ok` and `FAILED` in terminal codes.
    const match = cargoResultLine.exec(raw.replace(terminalCodes, "").trim());
    if (!match) continue;
    counts ??= { runner: "cargo-libtest", passed: 0, failed: 0, ignored: 0 };
    counts.passed += Number(match[1]);
    counts.failed += Number(match[2]);
    counts.ignored += Number(match[3]);
  }
  return counts;
}

/** The summary lines that report a focused stage's counts and the run's input fingerprint. */
export function focusedCountSummaryLines(result) {
  const focused = result.stages?.find((stage) => stage.name === "focused");
  if (!focused) return [];
  const lines = [];
  if (focused.tests) {
    const { passed, failed, ignored } = focused.tests;
    lines.push(`tests: passed=${passed} failed=${failed} ignored=${ignored}`);
  }
  const fingerprint = result.after ?? result.before ?? result.expectedFingerprint;
  if (fingerprint) lines.push(`input fingerprint: ${fingerprint}`);
  return lines;
}
