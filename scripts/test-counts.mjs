// Test counts for a focused launcher run.
//
// Owns: reading the wrapped test runner's own result lines from a focused
// stage's log and summing them into { passed, failed, ignored }, and the
// summary lines that report those counts. A focused run executes whatever
// follows `--`, so its exit status alone says nothing about how many tests ran;
// these counts are what lets the TermAl host credit a passing focused run from
// the run's own results.json (docs/test.md, "Which stages count as tests").
// Does not own: running the stage, the run records, or the host's credit rule
// (src/engram_launcher_stages.rs). Recognises libtest and complete native Node
// TAP/spec totals only; incomplete or ambiguous Node totals earn no counts.

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
  const lines = text.split(/\r?\n/u).map((raw) => raw.replace(terminalCodes, "").trim());
  let counts;
  for (const raw of lines) {
    // A forced-colour runner wraps `ok` and `FAILED` in terminal codes.
    const match = cargoResultLine.exec(raw.replace(terminalCodes, "").trim());
    if (!match) continue;
    counts ??= { runner: "cargo-libtest", passed: 0, failed: 0, ignored: 0 };
    counts.passed += Number(match[1]);
    counts.failed += Number(match[2]);
    counts.ignored += Number(match[3]);
  }
  const node = nativeNodeTestCounts(lines);
  // Mixed runner summaries cannot name one focused runner's execution.
  if (counts && node.seen) return undefined;
  return counts ?? node.counts;
}

/** Exactly one complete native TAP/spec footer, with coherent safe totals.
 * Failure diagnostics may follow the footer; they are not count evidence.
 */
function nativeNodeTestCounts(lines) {
  const keys = ["tests", "suites", "pass", "fail", "cancelled", "skipped", "todo", "duration_ms"];
  const candidates = [];
  const countLine = /^(#|ℹ) (tests|suites|pass|fail|cancelled|skipped|todo|duration_ms)\b/u;
  for (let index = 0; index < lines.length; index += 1) {
    if (countLine.test(lines[index])) candidates.push(index);
  }
  if (!candidates.length) return { seen: false };
  if (candidates.length !== keys.length) return { seen: true };
  const start = candidates[0];
  const prefix = lines[start].startsWith("# ") ? "#" : "ℹ";
  const values = {};
  for (let offset = 0; offset < keys.length; offset += 1) {
    if (candidates[offset] !== start + offset) return { seen: true };
    const key = keys[offset];
    const value = new RegExp(`^${prefix} ${key} (\\d+${key === "duration_ms" ? "(?:\\.\\d+)?" : ""})$`, "u")
      .exec(lines[start + offset]);
    if (!value) return { seen: true };
    values[key] = Number(value[1]);
    if (key === "duration_ms" ? !Number.isFinite(values[key]) : !Number.isSafeInteger(values[key])) {
      return { seen: true };
    }
  }
  const failed = values.fail + values.cancelled;
  const ignored = values.skipped + values.todo;
  const total = values.pass + failed + ignored;
  if (!Number.isSafeInteger(total) || total !== values.tests) return { seen: true };
  return { seen: true, counts: { runner: "node-test", passed: values.pass, failed, ignored } };
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
