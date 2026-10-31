// Pins the pure parts of the launcher's source record
// (test-launcher-source.mjs): rustup's active-toolchain line, a preflight's
// version line, whether a Cargo configuration selects the compiler, and the
// unknown record. The records a real run writes are covered in
// test-launcher.test.mjs. New file; the launcher's own test file is already
// large.
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import {
  bindSourceToInput,
  cargoConfigSelectsRustc,
  compilerWrapperInEnvironment,
  parseRustupActiveToolchain,
  preflightVersion,
  unknownSource,
} from "./test-launcher-source.mjs";
import { removeFixtureOnPass } from "./test-fixture-cleanup.mjs";
import { testTempDirectory } from "./test-temp-root.mjs";

test("rustup active toolchain: each known reason, and an unrecognized line kept unknown", () => {
  const name = "1.98.1-x86_64-pc-windows-msvc";
  for (const [reason, expected] of [
    ["(default)", ["default", "absent"]],
    ["(overridden by environment variable RUSTUP_TOOLCHAIN)", ["env", "absent"]],
    ["(directory override for 'F:\\GitHub\\TermAl')", ["directory override", "present"]],
    ["(overridden by '/Users/runner/TermAl/rust-toolchain.toml')", ["toolchain file", "present"]],
  ]) {
    assert.deepEqual(parseRustupActiveToolchain(`${name} ${reason}\n`),
      { resolvedToolchain: name, rustupReason: expected[0], rustupOverride: expected[1] });
  }
  // rustup's own chatter shares the log and is never read as the name.
  assert.deepEqual(parseRustupActiveToolchain(
    `info: syncing channel updates for '${name}'\ninfo: downloading component 'rustc'\n${name} (default)\n`),
  { resolvedToolchain: name, rustupReason: "default", rustupOverride: "absent" });
  assert.deepEqual(parseRustupActiveToolchain(`info: syncing channel updates\n${name}\nactive because: overridden\n`),
    { resolvedToolchain: null, rustupReason: "unrecognized", rustupOverride: "unknown" });
  assert.deepEqual(parseRustupActiveToolchain(""),
    { resolvedToolchain: null, rustupReason: "unrecognized", rustupOverride: "unknown" });
});

test("preflight version: the tool's own version line, never other output", () => {
  assert.equal(preflightVersion("\r\n  cargo 1.98.1 (abc 2026-01-01)\r\nnoise\n", "cargo"), "cargo 1.98.1 (abc 2026-01-01)");
  assert.equal(preflightVersion("info: syncing channel updates\nrustc 1.98.1 (x 2026-01-01)\n", "rustc"),
    "rustc 1.98.1 (x 2026-01-01)");
  assert.equal(preflightVersion("info: syncing channel updates\n", "cargo"), null);
  assert.equal(preflightVersion("rustc 1.98.1\n", "cargo"), null);
  assert.equal(preflightVersion(" \n\t\n"), null);
  assert.equal(preflightVersion(undefined, "cargo"), null);
});

test("source bound to the input: kept only for the fingerprint's own HEAD and status", () => {
  const empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
  const commit = "a".repeat(40);
  const { unknownReason: _none, ...base } = unknownSource(null);
  const source = { ...base, commit, tree: "b".repeat(40), clean: true };
  assert.equal(bindSourceToInput(source, { headCommit: commit, statusSha256: empty }), source);
  const dirty = { ...source, clean: false };
  assert.equal(bindSourceToInput(dirty, { headCommit: commit, statusSha256: "c".repeat(64) }), dirty);
  for (const input of [
    { headCommit: "d".repeat(40), statusSha256: empty },
    { headCommit: commit, statusSha256: "c".repeat(64) },
    undefined,
  ]) {
    const bound = bindSourceToInput(source, input);
    assert.equal(bound.unknownReason, "source_changed_during_measure");
    assert.equal(bound.clean, null);
    assert.equal(bound.commit, null);
  }
  const unknown = unknownSource("git_unavailable");
  assert.equal(bindSourceToInput(unknown, { headCommit: commit, statusSha256: empty }), unknown);
});

test("unknown source: identifies nothing, is never clean, and says why", () => {
  const source = unknownSource("source_unrecoverable");
  assert.deepEqual({ ...source, platform: undefined, measuredAt: undefined }, {
    sourceSchema: 1, commit: null, tree: null, clean: null, platform: undefined,
    launcherVersion: null, toolchain: null, measuredAt: undefined, unknownReason: "source_unrecoverable",
  });
  assert.ok(Number.isFinite(Date.parse(source.measuredAt)));
});

test("Cargo configuration: only allowlisted toolchain-neutral content is neutral, at the root, an ancestor or Cargo's home", (t) => {
  const base = mkdtempSync(join(testTempDirectory(), "launcher-source-"));
  removeFixtureOnPass(t, base);
  const root = join(base, "repo", "worktree");
  const home = join(base, "cargo-home");
  mkdirSync(root, { recursive: true });
  mkdirSync(home);
  // The ceiling keeps the walk inside this fixture: above it lies the host's
  // temporary directory, inside a user profile on Windows.
  const env = { CARGO_HOME: home, TERMAL_TEST_CARGO_CONFIG_CEILING: base };
  const write = (directory, text, name = "config.toml") => {
    mkdirSync(join(directory, ".cargo"), { recursive: true });
    writeFileSync(join(directory, ".cargo", name), text);
  };
  assert.equal(cargoConfigSelectsRustc(root, env), false);
  // Neutral: only allowlisted tables and [build] keys, comments, and values
  // continued over several lines.
  for (const text of [
    "[build]\njobs = 4\n[target.x86_64-pc-windows-msvc]\nrustc = \"not-build\"\n# rustc = \"comment\"\n",
    "[build]\ntarget-dir = \"t\"\nrustflags = [\n  \"-C\", \"target-cpu=native\",\n]\n",
    "[net]\nretry = 3\n[alias]\nb = \"build\"\n[profile.dev]\nopt-level = 1\n",
    "[target.'cfg(windows)']\nlinker = \"lld\"\ntarget.x.runner = { path = \"r\" }\n",
    "build.jobs = 2\nhttp.timeout = 30\n",
    // A comment after a header leaves a header the scanner can parse.
    "[build] # parallelism\njobs = 2\n",
  ]) {
    write(root, text);
    assert.equal(cargoConfigSelectsRustc(root, env), false, text);
  }
  for (const text of [
    // Not on the allowlist, whatever the route.
    "[build]\nrustc-wrapper = \"sccache\"\n", "[build]\nrustc-workspace-wrapper = \"w\"\n",
    "[unstable]\nconfig-include = true\n", "build = { jobs = 2 }\n", "[future]\nkey = 1\n","[build]\nrustc = \"x\"\n", "[ build ]\n\"rustc\" = 'x'\n",
    "[build]\n'rustc' = '/custom/compiler'\n", "[\"build\"]\nrustc = '/custom/compiler'\n",
    "build.rustc = \"x\"\n", "'build'.\"rustc\" = \"x\"\n", "build = { rustc = \"x\" }\n",
    // Included files are not followed, so any include counts as selecting.
    "include = \"more.toml\"\n", "include = [\"a.toml\", { path = \"b.toml\" }]\n",
    "[[include]]\npath = \"more.toml\"\n", "[include]\npath = \"more.toml\"\n",
    // Cargo passes [env] to the rustc it runs, so it can change the toolchain.
    "[env]\nRUSTUP_TOOLCHAIN = { value = \"nightly\", force = true }\n",
    "env.RUSTUP_TOOLCHAIN = \"nightly\"\n", "[env.RUSTUP_TOOLCHAIN]\nvalue = \"nightly\"\n",
    // Multiline strings are not followed: a fake header inside one must not
    // hide a later build.rustc, so any multiline string counts as selecting.
    "[build]\nrustdocflags = \"\"\"\n[target]\n\"\"\"\nrustc = \"/custom/rustc\"\n",
    "[build]\nrustdocflags = '''\n[target]\n'''\nrustc = '/custom/rustc'\n",
    // A line that opens like a header but does not parse as one (here a
    // quoted segment holding '#', cut by comment stripping) leaves the table
    // unknown, so it counts as selecting rather than reusing the last table.
    "[net]\nretry = 3\n[build.\"x#y\"]\nrustc = \"/custom/rustc\"\n"]) {
    write(root, text);
    assert.equal(cargoConfigSelectsRustc(root, env), true, text);
  }
  write(root, "");
  write(join(base, "repo"), "[build]\nrustc = \"x\"\n", "config");
  assert.equal(cargoConfigSelectsRustc(root, env), true, "an ancestor's legacy config");
  // The walk stops at the ceiling: the same ancestor above it is not read.
  assert.equal(cargoConfigSelectsRustc(root, { ...env, TERMAL_TEST_CARGO_CONFIG_CEILING: root }), false,
    "an ancestor above the ceiling");
  // Only an absolute ceiling counts; a relative one leaves the walk whole.
  assert.equal(cargoConfigSelectsRustc(root, { ...env, TERMAL_TEST_CARGO_CONFIG_CEILING: "worktree" }), true,
    "a relative ceiling");
  write(join(base, "repo"), "", "config");
  writeFileSync(join(home, "config.toml"), "[build]\nrustc = \"x\"\n");
  assert.equal(cargoConfigSelectsRustc(root, env), true, "Cargo's home");
  writeFileSync(join(home, "config.toml"), "");
  // A relative Cargo home is the run root's, as Cargo (run there) reads it,
  // whatever this process's working directory is.
  const relativeHome = join(root, "relative-cargo-home");
  mkdirSync(relativeHome);
  writeFileSync(join(relativeHome, "config.toml"), "[build]\nrustc = \"x\"\n");
  assert.notEqual(process.cwd(), root);
  assert.equal(cargoConfigSelectsRustc(root, {
    CARGO_HOME: "relative-cargo-home", TERMAL_TEST_CARGO_CONFIG_CEILING: base,
  }), true, "relative home");
});

test("compiler wrapper in the environment: any of Cargo's wrapper variables, in any case, when set", () => {
  assert.equal(compilerWrapperInEnvironment({}), false);
  assert.equal(compilerWrapperInEnvironment({ RUSTC: "rustc", RUSTUP_TOOLCHAIN: "stable", CARGO_BUILD_RUSTC: "r" }), false);
  assert.equal(compilerWrapperInEnvironment({ RUSTC_WRAPPER: " " }), false);
  for (const name of ["RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", "rustc_wrapper"]) {
    assert.equal(compilerWrapperInEnvironment({ [name]: "sccache" }), true, name);
  }
});
