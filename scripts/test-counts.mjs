// Test counts for a focused launcher run.
//
// Owns: reading the wrapped test runner's own result lines from a focused
// stage's log and summing them into { passed, failed, ignored }, and the
// summary lines that report those counts. A focused run executes whatever
// follows `--`, so its exit status alone says nothing about how many tests ran;
// these counts are what lets the TermAl host credit a passing focused run from
// the run's own results.json (docs/test.md, "Which stages count as tests").
// Also owns: working out which test-thread setting a focused Rust run used, and
// its summary line. A direct `cargo test` skips scripts/test-rust.sh, so on a
// many-core host it runs far more threads than a gate (docs/test.md).
// Does not own: running the stage, the run records, or the host's credit rule
// (src/engram_launcher_stages.rs). Recognises libtest and complete native Node
// TAP/spec totals only; incomplete or ambiguous Node totals earn no counts.

import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";

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

/** A count as Rust parses it for libtest, or `null` where libtest would refuse it. */
function libtestThreadCount(value) {
  if (!/^\+?\d+$/u.test(String(value))) return null;
  const count = Number(value);
  return Number.isSafeInteger(count) && count > 0 ? count : null;
}

/** The harness options that consume the next argument, so it can't be read as an option. */
const harnessValueOptions = new Set(["--skip", "--logfile", "--color", "--format", "-Z", "--shuffle-seed"]);
/** Cargo's global options that consume the next argument, ahead of its subcommand. */
const cargoValueOptions = new Set(["--config", "--color", "-Z", "-C"]);

/** The arguments of a direct `cargo test`, or `undefined` for any other command. */
function directCargoTestArguments(command, args) {
  const program = String(command).split(/[\\/]/u).pop();
  if (!/^cargo(?:\.exe)?$/iu.test(program)) return undefined;
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (cargoValueOptions.has(arg)) index += 1;
    else if (!arg.startsWith("-") && !arg.startsWith("+")) return arg === "test" ? args.slice(index + 1) : undefined;
  }
  return undefined;
}

/** Every `--test-threads` value libtest would see among a `cargo test`'s arguments. */
function harnessThreadOptions(args) {
  const separator = args.indexOf("--");
  const harness = separator < 0 ? [] : args.slice(separator + 1);
  const values = [];
  for (let index = 0; index < harness.length; index += 1) {
    const arg = harness[index];
    if (arg === "--") break;
    if (harnessValueOptions.has(arg)) index += 1;
    else if (arg.startsWith("--test-threads=")) values.push(arg.slice("--test-threads=".length));
    else if (arg === "--test-threads") values.push(harness[index += 1]);
  }
  return values;
}

/** The values of an environment variable, under every spelling that names it here. */
function environmentValues(env, name, platform) {
  return Object.keys(env)
    .filter((key) => (platform === "win32" ? key.toUpperCase() === name : key === name))
    .map((key) => env[key]);
}

/** The text of every Cargo configuration file that applies at `cwd`. */
export function cargoConfigTexts(cwd, env = process.env) {
  const directories = [];
  for (let directory = resolve(cwd); ; directory = dirname(directory)) {
    directories.push(join(directory, ".cargo"));
    if (directory === dirname(directory)) break;
  }
  directories.push(env.CARGO_HOME ?? join(homedir(), ".cargo"));
  return directories.flatMap((directory) => ["config", "config.toml"]
    .map((name) => join(directory, name))
    .filter((path) => existsSync(path))
    .map((path) => {
      try {
        return readFileSync(path, "utf8");
      } catch {
        return "RUST_TEST_THREADS: unreadable, so it can't be ruled out";
      }
    }));
}

/** Whether argv itself runs the gate's wrapper, in a form the launcher builds. */
function runsTestRustWrapper(args) {
  if (args[0] === "-c") return /^exec (?:\.\/)?scripts\/test-rust\.sh(?: [^;&|`$()]*)?$/u.test(args[1] ?? "");
  return /(?:^|[\\/])scripts[\\/]test-rust\.sh$/u.test(args[0] ?? "");
}

/**
 * The test-thread setting of a focused Rust stage, or `undefined` for a stage
 * that shows no sign of running libtest. It reports what the launcher can see,
 * and nothing it can't: only a direct `cargo test`, or the gate's wrapper run
 * directly, has arguments and an environment the launcher knows, so any other
 * command that runs libtest is `unknown`, and so is a setting whose effect isn't
 * certain, such as Cargo configuration that names the variable.
 * `libtest-default` means none of that set a count, so libtest chooses from the
 * parallelism it finds; `availableParallelism` is the launcher's own reading of
 * that. An aborted run prints no result line, so a direct `cargo test` counts
 * without one, and so does one that ran no test.
 */
export function focusedTestThreads({
  command, args = [], env = {}, log = "", counts, availableParallelism, platform = process.platform, cargoConfigs = [],
}) {
  const unknown = { source: "unknown", threads: null };
  const cargoSetsThreads = cargoConfigs.some((text) => text.includes("RUST_TEST_THREADS"));
  const wrapperLines = [...log.matchAll(/^Rust test gate: .*test threads=(\d+)/gmu)];
  if (wrapperLines.length) {
    // The wrapper only exports the variable: a harness option it forwards wins.
    return wrapperLines.length === 1 && runsTestRustWrapper(args) && !cargoSetsThreads &&
      !args.some((arg) => String(arg).includes("--test-threads") || String(arg).includes("--config"))
      ? { source: "scripts/test-rust.sh", threads: Number(wrapperLines[0][1]) }
      : unknown;
  }
  const cargoArgs = directCargoTestArguments(command, args);
  if (!cargoArgs) {
    return counts?.runner === "cargo-libtest" || /^running \d+ tests?$/mu.test(log) ? unknown : undefined;
  }
  const options = harnessThreadOptions(cargoArgs);
  if (options.length > 1) return unknown;
  if (options.length === 1) return { source: "--test-threads", threads: libtestThreadCount(options[0]) };
  // Cargo can set the variable itself, which the launcher's environment won't show.
  if (cargoSetsThreads || args.includes("--config") || args.some((arg) => String(arg).startsWith("--config="))) return unknown;
  const values = environmentValues(env, "RUST_TEST_THREADS", platform);
  if (values.length > 1) return unknown;
  if (values.length === 1) return { source: "RUST_TEST_THREADS", threads: libtestThreadCount(values[0]) };
  return { source: "libtest-default", threads: null, availableParallelism };
}

/** The summary lines that report focused counts; old digests remain attributed history. */
export function focusedCountSummaryLines(result) {
  const focused = result.stages?.find((stage) => stage.name === "focused");
  if (!focused) return [];
  const lines = [];
  if (focused.tests) {
    const { passed, failed, ignored } = focused.tests;
    lines.push(`tests: passed=${passed} failed=${failed} ignored=${ignored}`);
  }
  if (focused.testThreads) {
    const { source, threads, availableParallelism } = focused.testThreads;
    lines.push(
      source === "unknown"
        ? "test threads: unknown; this command's setting isn't visible to the launcher, so read it from the test process"
        : source === "libtest-default"
          ? `test threads: none set, so libtest chooses; the launcher sees ${availableParallelism} available`
          : `test threads: ${threads ?? "not a valid count"} (${source})`,
    );
  }
  if (!Object.hasOwn(result, "sourceIdBefore") && !Object.hasOwn(result, "sourceIdAfter")) {
    const fingerprint = result.after ?? result.before ?? result.expectedFingerprint;
    if (fingerprint) lines.push(`historical input fingerprint: ${fingerprint}`);
  }
  return lines;
}
