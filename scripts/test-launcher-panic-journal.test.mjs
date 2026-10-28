// Owns the launcher's side of the Rust panic journal: the environment it
// gives a run's commands (RUST_BACKTRACE, and the journal path the test
// binary's hook writes to, src/test_panic_journal.rs), and the summary lines
// it prints for a Rust stage that ended abnormally. Does not own the hook
// itself, which the Rust regression in src/tests/test_panic_journal.rs
// covers. New file, split from nothing.
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import {
  PANIC_JOURNAL_ENV,
  PANIC_JOURNAL_FILE,
  panicJournalSummaryLines,
  runEnvironment,
  summarize,
} from "./test-launcher.mjs";
import { testTempDirectory } from "./test-temp-root.mjs";

function runDirectory(t) {
  const runDir = mkdtempSync(join(testTempDirectory(), "launcher-panic-journal-"));
  t.after(() => rmSync(runDir, { recursive: true, force: true }));
  return runDir;
}

let sequence = 0;

// One panic as the hook writes it: the panic line, then its thread line,
// joined by process id and sequence.
function record(thread, message, frames = 3) {
  const seq = sequence++;
  return [
    JSON.stringify({
      pid: 1,
      seq,
      at_unix_ms: 1793200000000,
      message,
      location: "src\\tests\\example.rs:10:5",
      backtrace: Array.from({ length: frames }, (_, index) => `   ${index}: frame_${index}`).join("\n"),
    }),
    JSON.stringify({ pid: 1, seq, thread }),
  ].join("\n");
}

function rustStage(runDir, logText, fields = {}) {
  const log = join(runDir, "rust-tests.log");
  writeFileSync(log, logText);
  return { name: "rust-tests", state: "failed", code: 101, log, ...fields };
}

const abortLine =
  "error: test failed, to rerun pass `--bin termal`\n" +
  "Caused by:\n  process didn't exit successfully: `target\\debug\\deps\\termal-1.exe` " +
  "(exit code: 0xc0000409, STATUS_STACK_BUFFER_OVERRUN)\n";
const ordinaryFailureLine =
  "error: test failed, to rerun pass `--bin termal`\n" +
  "Caused by:\n  process didn't exit successfully: `target/debug/deps/termal-1` (exit status: 101)\n";
const buildScriptFailureLine =
  "error: failed to run custom build command for `termal v0.1.0`\n" +
  "Caused by:\n  process didn't exit successfully: `target\\debug\\build\\termal-2\\build-script-build` (exit code: 1)\n";

test("a run's commands get panic backtraces only, and the run's journal path", () => {
  const env = runEnvironment({ PATH: "/bin" }, "/runs/run-1", "linux");
  assert.equal(env.RUST_BACKTRACE, "1");
  assert.equal(env.RUST_LIB_BACKTRACE, "0", "library backtraces stay off, so anyhow errors capture no stack");
  assert.equal(env[PANIC_JOURNAL_ENV], join("/runs/run-1", PANIC_JOURNAL_FILE));
  assert.equal(env.PATH, "/bin");
});

test("a caller's own backtrace variables are kept, in any case on Windows", () => {
  const panicOnly = runEnvironment({ RUST_BACKTRACE: "0" }, "/r", "linux");
  assert.equal(panicOnly.RUST_BACKTRACE, "0");
  assert.equal(panicOnly.RUST_LIB_BACKTRACE, undefined, "a caller who chose RUST_BACKTRACE keeps std's pairing");
  assert.equal(runEnvironment({ RUST_BACKTRACE: "" }, "/r", "linux").RUST_BACKTRACE, "");
  const libraryOnly = runEnvironment({ RUST_LIB_BACKTRACE: "1" }, "/r", "linux");
  assert.equal(libraryOnly.RUST_BACKTRACE, "1");
  assert.equal(libraryOnly.RUST_LIB_BACKTRACE, "1");
  const windows = runEnvironment({ Rust_Backtrace: "full" }, "C:\\r", "win32");
  assert.equal(windows.Rust_Backtrace, "full");
  assert.equal(windows.RUST_BACKTRACE, undefined, "no second spelling of the caller's variable is added");
  assert.equal(windows.RUST_LIB_BACKTRACE, undefined);
  const windowsLibrary = runEnvironment({ rust_lib_backtrace: "1" }, "C:\\r", "win32");
  assert.equal(windowsLibrary.RUST_BACKTRACE, "1");
  assert.equal(windowsLibrary.RUST_LIB_BACKTRACE, undefined, "the caller's spelling of it is the one kept");
});

test("on Windows the journal variable replaces any spelling of it, never duplicates it", () => {
  const env = runEnvironment({ termal_test_panic_journal: "elsewhere" }, "C:\\runs\\run-2", "win32");
  const spellings = Object.keys(env).filter((key) => key.toUpperCase() === PANIC_JOURNAL_ENV);
  assert.deepEqual(spellings, [PANIC_JOURNAL_ENV]);
  assert.equal(env[PANIC_JOURNAL_ENV], join("C:\\runs\\run-2", PANIC_JOURNAL_FILE));
});

test("an aborted Rust stage lists the last three recorded panics with the journal path", (t) => {
  const runDir = runDirectory(t);
  const journal = join(runDir, PANIC_JOURNAL_FILE);
  writeFileSync(journal, [
    record("tests::a", "first"),
    record("tests::b", "second"),
    "{\"cut short by the abort",
    record("tests::c", "third", 20),
    record("tests::d", "fourth"),
    "",
  ].join("\n"));
  const lines = panicJournalSummaryLines({ panicJournal: journal, stages: [rustStage(runDir, abortLine)] }, runDir);
  assert.equal(lines[0], `rust panic journal: ${journal} (4 recorded; the last 3 follow)`);
  assert.ok(lines.includes("  panic in tests::b at src\\tests\\example.rs:10:5: second"));
  assert.ok(lines.includes("  panic in tests::d at src\\tests\\example.rs:10:5: fourth"));
  assert.ok(!lines.some((line) => line.includes("tests::a")), "only the last three are listed");
  assert.ok(lines.includes("    [backtrace truncated; full record in the journal]"));
});

test("an ordinary test failure, a pass, a build-script failure and a non-Rust failure list no journal", (t) => {
  const runDir = runDirectory(t);
  writeFileSync(join(runDir, PANIC_JOURNAL_FILE), `${record("tests::a", "expected")}\n`);
  assert.deepEqual(panicJournalSummaryLines({ stages: [rustStage(runDir, ordinaryFailureLine)] }, runDir), []);
  assert.deepEqual(panicJournalSummaryLines({ stages: [rustStage(runDir, buildScriptFailureLine)] }, runDir), []);
  assert.deepEqual(panicJournalSummaryLines({ stages: [{ name: "rust-tests", state: "passed", code: 0 }] }, runDir), []);
  assert.deepEqual(panicJournalSummaryLines({
    stages: [{ name: "focused", state: "failed", code: 1, command: ["node", "x.mjs"] }],
  }, runDir), []);
});

test("a focused cargo run or a direct test-binary run that crashes is abnormal too", (t) => {
  const runDir = runDirectory(t);
  writeFileSync(join(runDir, PANIC_JOURNAL_FILE), `${record("tests::x", "boom")}\n`);
  const commands = [
    ["C:\\Users\\u\\.cargo\\bin\\cargo.exe", "test"],
    ["target\\debug\\deps\\termal-1.exe", "--exact", "tests::x"],
  ];
  for (const command of commands) {
    for (const fields of [{ code: null, signal: "SIGABRT" }, { code: 3221226505 }]) {
      const stage = { name: "focused", state: "failed", command, ...fields };
      const lines = panicJournalSummaryLines({ stages: [stage] }, runDir);
      assert.match(lines[0] ?? "", /^rust panic journal: .* \(1 recorded; the last 1 follow\)$/u, JSON.stringify(stage));
      assert.ok(lines.includes("  panic in tests::x at src\\tests\\example.rs:10:5: boom"), lines.join("\n"));
    }
  }
});

test("a panic whose thread line the abort cut off is still listed, its thread unknown", (t) => {
  const runDir = runDirectory(t);
  const journal = join(runDir, PANIC_JOURNAL_FILE);
  const [panicLine] = record("tests::lost", "kept anyway").split("\n");
  writeFileSync(journal, `${panicLine}\n`);
  const lines = panicJournalSummaryLines({ panicJournal: journal, stages: [rustStage(runDir, abortLine)] }, runDir);
  assert.ok(lines.includes("  panic in <unknown thread> at src\\tests\\example.rs:10:5: kept anyway"), lines.join("\n"));
});

test("an aborted Rust stage with no journal says that none was recorded", (t) => {
  const runDir = runDirectory(t);
  const lines = panicJournalSummaryLines({ stages: [rustStage(runDir, abortLine)] }, runDir);
  assert.deepEqual(lines, [
    `rust panic journal: ${join(runDir, PANIC_JOURNAL_FILE)} (none recorded; the test binary wrote no panic before it ended)`,
  ]);
});

test("the run summary carries the journal lines of an aborted Rust stage", async (t) => {
  const runDir = runDirectory(t);
  const journal = join(runDir, PANIC_JOURNAL_FILE);
  writeFileSync(journal, `${record("tests::aborting", "the first panic")}\n`);
  writeFileSync(join(runDir, "results.json"), JSON.stringify({
    runId: "test-panic-journal",
    state: "failed",
    exitCode: 1,
    ended: "2026-10-28T16:00:01.000Z",
    panicJournal: journal,
    stages: [rustStage(runDir, abortLine)],
  }));
  const summary = await summarize(runDir);
  assert.match(summary, /^rust panic journal: .* \(1 recorded; the last 1 follow\)$/mu);
  assert.match(summary, /^ {2}panic in tests::aborting at src\\tests\\example\.rs:10:5: the first panic$/mu);
});
