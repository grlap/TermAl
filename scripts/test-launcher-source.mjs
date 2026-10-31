// Clean-start source metadata for the shared test launcher
// (scripts/test-launcher.mjs, docs/test.md "Source metadata").
// Owns: what a run records about the source it started from (commit, tree,
// whether the checkout was clean, the platform, the launcher's own blob at
// HEAD) and the Rust toolchain record built from the launcher's preflight
// output. Does not own running any preflight or stage, the run directory,
// the review-freeze fingerprint (review-freeze-fingerprint.mjs), or any
// judgement of a run: these are observations a reader checks, never credit.
// New file; not split from an existing one.
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { isolatedGitEnvironment } from "./review-freeze-fingerprint.mjs";

export const SOURCE_SCHEMA = 1;
const launcherPath = "scripts/test-launcher.mjs";

/** The host OS and CPU in the names a reader on any platform compares. */
export function sourcePlatform(platform = process.platform, arch = process.arch) {
  return {
    os: { win32: "windows", darwin: "macos", linux: "linux" }[platform] ?? platform,
    arch: { x64: "x86_64", arm64: "aarch64", ia32: "x86" }[arch] ?? arch,
  };
}

// One Git query. `missing` is a query that ran and found nothing (a path not
// at HEAD); `unavailable` is Git that could not run at all.
function git(root, env, args) {
  const outcome = spawnSync("git", args, {
    cwd: root,
    env,
    encoding: "utf8",
    windowsHide: true,
    maxBuffer: 64 * 1024 * 1024,
  });
  // Output past the buffer is Git that ran: its answer is unreadable, not Git
  // missing.
  if (outcome.error?.code === "ENOBUFS") return { missing: true };
  if (outcome.error) return { unavailable: true };
  if (outcome.status !== 0) return { missing: true };
  return { stdout: outcome.stdout };
}

const objectId = (text) => {
  const value = text.replace(/\r?\n$/u, "");
  return /^[a-f0-9]{40}(?:[a-f0-9]{24})?$/u.test(value) ? value : null;
};

/** A source record that identifies nothing, saying why. Never clean. */
export function unknownSource(reason) {
  return {
    sourceSchema: SOURCE_SCHEMA,
    commit: null,
    tree: null,
    clean: null,
    platform: sourcePlatform(),
    launcherVersion: null,
    toolchain: null,
    measuredAt: new Date().toISOString(),
    unknownReason: reason,
  };
}

// The SHA-256 of no output: a `git status` that lists nothing.
const emptySha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/**
 * Keeps `source` only when it describes the input the run's fingerprint
 * froze (review-freeze-fingerprint.mjs `captureFingerprint`): the same HEAD,
 * and clean exactly when the fingerprint's own `git status` listed nothing.
 * Otherwise the checkout changed between the two measurements, and the
 * record identifies nothing.
 */
export function bindSourceToInput(source, input) {
  if (source?.unknownReason) return source;
  const inputClean = input?.statusSha256 === emptySha256;
  return source && source.commit === input?.headCommit && source.clean === inputClean
    ? source
    : unknownSource("source_changed_during_measure");
}

/**
 * The source a run starts from, measured once with Git under the launcher's
 * isolated configuration (no system or global Git config) before any stage. `clean` is true only when `git status` lists nothing, untracked
 * files included; a checkout Git cannot describe is never clean: its
 * identity fields are null and `unknownReason` says why.
 */
export async function captureSourceMetadata(root, env = process.env) {
  const gitEnv = isolatedGitEnvironment(env);
  const { unknownReason: _none, ...source } = unknownSource(null);
  const unknown = (reason) => ({ ...source, unknownReason: reason });
  const head = git(root, gitEnv, ["rev-parse", "--verify", "HEAD^{commit}"]);
  if (head.unavailable) return unknown("git_unavailable");
  const commit = head.missing ? null : objectId(head.stdout);
  if (!commit) return unknown("head_unavailable");
  const tree = git(root, gitEnv, ["rev-parse", "--verify", `${commit}^{tree}`]);
  const treeId = tree.stdout === undefined ? null : objectId(tree.stdout);
  if (!treeId) return unknown("tree_unavailable");
  const status = git(root, gitEnv, ["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
  if (status.stdout === undefined) return unknown("status_unavailable");
  const launcher = git(root, gitEnv, ["rev-parse", "--verify", "--quiet", `${commit}:${launcherPath}`]);
  return {
    ...source,
    commit,
    tree: treeId,
    clean: status.stdout.length === 0,
    launcherVersion: launcher.stdout === undefined ? null : objectId(launcher.stdout),
  };
}

const rustupReasons = [
  [/^\(default\)$/u, "default", "absent"],
  [/^\(overridden by environment variable RUSTUP_TOOLCHAIN\)$/u, "env", "absent"],
  [/^\(directory override for '.*'\)$/u, "directory override", "present"],
  [/^\(overridden by '.*'\)$/u, "toolchain file", "present"],
];

/**
 * Reads `rustup show active-toolchain`'s first line, `NAME (REASON)`. A line
 * in another shape keeps its name but is not classified: its override state
 * is unknown rather than guessed.
 */
export function parseRustupActiveToolchain(text) {
  // The log holds stdout and stderr together; rustup's own chatter
  // ("info: syncing channel updates ...") is skipped, never read as a name.
  const lines = (text ?? "").split(/\r?\n/u).map((entry) => entry.trim()).filter(Boolean);
  for (const line of lines) {
    const match = /^([A-Za-z0-9][\w.+-]*)\s+(\(.*\))$/u.exec(line);
    if (!match) continue;
    for (const [pattern, reason, override] of rustupReasons) {
      if (pattern.test(match[2])) {
        return { resolvedToolchain: match[1], rustupReason: reason, rustupOverride: override };
      }
    }
  }
  return { resolvedToolchain: null, rustupReason: "unrecognized", rustupOverride: "unknown" };
}

// Whether a Cargo configuration file this run's Cargo reads may change the
// compiler or toolchain it runs, beyond what the probes see: the run root's
// and each ancestor's `.cargo/config.toml` and `.cargo/config`, then Cargo's
// home. An ALLOWLIST, not a list of known routes: a file is neutral only when
// every table and key in it is on the short toolchain-neutral list below, so
// any route not named there (build.rustc, a rustc wrapper, [env], include,
// [unstable], an inline build table, ...) counts as changing it. Read only; a
// file that cannot be read counts too, so the toolchain record never names a
// compiler Cargo may not have used.
//
// TERMAL_TEST_CARGO_CONFIG_CEILING is a test seam: an absolute directory at
// which the ancestor walk stops, so launcher fixtures under a temporary
// directory inside a user profile never read that host's own configuration.
// Real runs leave it unset, and every ancestor counts, as Cargo reads them all.
export function cargoConfigSelectsRustc(root, env = process.env) {
  const ceilingSetting = env.TERMAL_TEST_CARGO_CONFIG_CEILING;
  const ceiling = typeof ceilingSetting === "string" && isAbsolute(ceilingSetting)
    ? resolve(ceilingSetting)
    : undefined;
  const directories = [];
  for (let directory = resolve(root); ; directory = dirname(directory)) {
    directories.push(join(directory, ".cargo"));
    if (directory === ceiling || dirname(directory) === directory) break;
  }
  const cargoHome = Object.entries(env).find(([key]) => key.toUpperCase() === "CARGO_HOME")?.[1];
  // Cargo runs with the run root as its working directory, so a relative
  // home is relative to the root, never to this process.
  directories.push(cargoHome ? resolve(root, cargoHome) : join(homedir(), ".cargo"));
  for (const directory of directories) {
    for (const name of ["config.toml", "config"]) {
      const path = join(directory, name);
      if (!existsSync(path)) continue;
      let text;
      try {
        text = readFileSync(path, "utf8");
      } catch {
        return true;
      }
      if (!tomlToolchainNeutral(text)) return true;
    }
  }
  return false;
}

// Top-level tables whose keys never choose the compiler or its environment.
const neutralTables = new Set([
  "alias", "cargo-new", "http", "net", "profile", "registries", "registry", "source", "term", "target",
]);
// `[build]` keys that never choose the compiler or its environment.
const neutralBuildKeys = new Set(["jobs", "target", "target-dir", "incremental", "rustflags", "rustdocflags"]);

// A dotted path of bare, "basic" or 'literal' key segments.
const keySegment = String.raw`(?:"[^"]*"|'[^']*'|[A-Za-z0-9_-]+)`;
const keyPath = new RegExp(String.raw`^(${keySegment}(?:\s*\.\s*${keySegment})*)\s*=\s*(.*)$`, "u");
const segments = (path) => (path.match(new RegExp(keySegment, "gu")) ?? [])
  .map((segment) => segment.replace(/^["']|["']$/gu, ""));

function neutralPath(path) {
  if (path.length === 0) return false;
  if (neutralTables.has(path[0])) return true;
  return path[0] === "build" && path.length >= 2 && neutralBuildKeys.has(path[1]);
}

// Whether every table header and key assignment in a Cargo configuration is
// on the neutral list. Lines that are neither (an array value continuing over
// several lines) assign nothing and are skipped. This line scanner does not
// parse all of TOML, so it fails closed on what it cannot follow: a multiline
// string, whose lines could look like headers or keys, and a line that opens
// like a header but does not parse as one, after which the current table
// would be unknown.
function tomlToolchainNeutral(text) {
  if (/"""|'''/u.test(text)) return false;
  let table = [];
  for (const raw of text.split(/\r?\n/u)) {
    const line = raw.replace(/#.*$/u, "").trim();
    if (!line) continue;
    const header = /^\[{1,2}\s*([^\]]+?)\s*\]{1,2}$/u.exec(line);
    if (header) {
      table = segments(header[1]);
      // `[build]` itself is neutral; its keys are judged one by one.
      if (!(table.length === 1 && table[0] === "build") && !neutralPath(table)) return false;
      continue;
    }
    if (line.startsWith("[")) return false;
    const assignment = keyPath.exec(line);
    if (!assignment) continue;
    const path = [...table, ...segments(assignment[1])];
    if (!neutralPath(path)) return false;
    // An inline table's own keys are not judged here: only a neutral table
    // may take one.
    if (/^\{/u.test(assignment[2]) && !neutralTables.has(path[0])) return false;
  }
  return true;
}

// Environment that makes Cargo run its compiler through another program,
// which the probes would not run. RUSTC and CARGO_BUILD_RUSTC are not here:
// the probe runs exactly that compiler. RUSTUP_TOOLCHAIN is not here: the
// probes run with this same environment, so they see the same toolchain.
const toolchainWrapperVariables = [
  "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
  "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
];

/** Whether the run's environment sets a compiler wrapper. */
export function compilerWrapperInEnvironment(env = process.env) {
  return Object.entries(env).some(([key, value]) =>
    toolchainWrapperVariables.includes(key.toUpperCase()) && typeof value === "string" && value.trim() !== "");
}

/**
 * The version line a `--version` probe printed for `tool` (`cargo 1.98.1 …`,
 * `rustc 1.98.1 …`), skipping anything else the log holds (it holds stdout
 * and stderr together); null when there is none. Without a tool, the first
 * nonblank line.
 */
export function preflightVersion(text, tool) {
  const lines = (text ?? "").split(/\r?\n/u).map((line) => line.trim()).filter(Boolean);
  if (!tool) return lines[0] ?? null;
  return lines.find((line) => line.startsWith(`${tool} `) && /^\S+ \d/u.test(line)) ?? null;
}

/**
 * The toolchain record before its preflight has succeeded: the executable
 * chosen and where it came from, with no version claimed.
 */
export function pendingToolchain(cargoExecutable, source) {
  return {
    cargoVersion: null,
    rustcVersion: null,
    cargoExecutable,
    source,
    rustupOverride: null,
    resolvedToolchain: null,
    rustupReason: null,
    unknownReason: "preflight_failed",
  };
}
