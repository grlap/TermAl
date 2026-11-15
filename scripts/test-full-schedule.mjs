// Opt-in full-stage scheduling and its private filesystem environments.
// Commands, test selection, result publication and source identity stay owned
// by test-launcher.mjs; process groups stay owned by test-process-owner.mjs.
import { lstatSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { isolatedGitEnvironment } from "./review-freeze-fingerprint.mjs";

export const PARALLEL_FULL_SCHEDULE = "dl560-rust-vitest-v1";
// App-specific digest only: never publish the native machine-id. No CLI or
// environment value can extend this allowlist.
const DL560_MACHINE_HASHES = Object.freeze([
  "51166a2ab12506f502bb370a42cfb2e92177bd3a8fd8821f0d03ba6e0e827f41",
]);
const stageNames = ["fmt", "cargo-check", "typescript", "fingerprint-tests", "rust-tests", "vitest"];

// Pure observation assessment is separately testable on every host. Native
// admission below always supplies its own platform, read and committed list.
export function parallelMachineHash(machineId) {
  const id = typeof machineId === "string" ? machineId.split("\n", 1)[0].trim() : "";
  if (!/^[a-f0-9]{32}$/u.test(id)) return undefined;
  return createHash("sha256").update(`termal-parallel-full-stages:${id}`).digest("hex");
}

export function assertParallelHostObservation(platform, machineId, allowedHashes) {
  const hash = parallelMachineHash(machineId);
  if (platform !== "linux" || hash === undefined || !allowedHashes.includes(hash)) {
    throw new Error("parallel Rust/Vitest FULL requires the allowlisted native DL560 Linux host");
  }
}

export function assertParallelFullHost() {
  if (process.platform !== "linux") {
    throw new Error("parallel Rust/Vitest FULL requires the allowlisted native DL560 Linux host");
  }
  let machineId;
  try { machineId = readFileSync("/etc/machine-id", "utf8"); }
  catch { throw new Error("DL560 machine identity is unavailable; parallel FULL refused"); }
  assertParallelHostObservation(process.platform, machineId, DL560_MACHINE_HASHES);
}

// The marker is part of the existing immutable stages array, so registration
// binds the schedule without introducing an unbound request-level option.
export function fullSchedule(stages, full) {
  if (!stages.some((stage) => stage.schedule !== undefined)) return undefined;
  if (!full) throw new Error("parallel schedule is supported only by full runs");
  if (stages.length !== stageNames.length || stages.some((stage, i) =>
    stage.name !== stageNames[i] || stage.schedule !== (i >= 4 ? PARALLEL_FULL_SCHEDULE : undefined))) {
    throw new Error("invalid parallel full stage schedule");
  }
  return PARALLEL_FULL_SCHEDULE;
}

export async function runStageSchedule(count, runStage, { parallel = false, beforeParallel, onFatal } = {}) {
  if (!parallel) {
    for (let index = 0; index < count; index += 1) {
      if (await runStage(index) === false) break;
    }
    return;
  }
  if (count !== stageNames.length) throw new Error("invalid parallel full stage count");
  for (let index = 0; index < 4; index += 1) await runStage(index);
  await beforeParallel?.([4, 5]);
  const settled = await Promise.allSettled([4, 5].map(async (index) => {
    try { return await runStage(index); }
    catch (error) { onFatal?.(error); throw error; }
  }));
  const failed = settled.find((result) => result.status === "rejected");
  if (failed) throw failed.reason;
}

function plainDirectory(path) {
  try { mkdirSync(path, { mode: 0o700 }); }
  catch (error) { if (error.code !== "EEXIST") throw error; }
  const entry = lstatSync(path);
  if (entry.isSymbolicLink() || !entry.isDirectory()) throw new Error(`expected plain directory: ${path}`);
  return path;
}

// Read-only validation: inspect every existing component before making output
// or private stage directories. Walk the raw path so a link followed by '..'
// cannot disappear through lexical normalization before it is inspected.
export function validateCargoOutputTarget(root, target) {
  root = resolve(root);
  if (typeof target !== "string" || !target || target.includes("\0")) {
    throw new Error("parallel FULL Cargo output target is unavailable");
  }
  const canonicalRoot = realpathSync.native(root);
  const raw = isAbsolute(target) ? target : `${root}${sep}${target}`;
  const path = process.platform === "win32" ? raw.replaceAll("/", sep) : raw;
  const folded = (value) => process.platform === "win32" ? value.toLowerCase() : value;
  const prefix = `${root}${sep}`;
  if (!folded(path).startsWith(folded(prefix))) {
    throw new Error("parallel FULL requires Cargo output inside its worktree");
  }
  const contained = (value) => {
    const difference = relative(canonicalRoot, value);
    return difference !== ".." && !difference.startsWith(`..${sep}`) && !isAbsolute(difference);
  };
  let current = canonicalRoot;
  for (const part of path.slice(prefix.length).split(sep)) {
    if (!part || part === ".") continue;
    current = part === ".." ? dirname(current) : join(current, part);
    if (!contained(current)) throw new Error("parallel FULL Cargo output escapes its worktree");
    let entry;
    try { entry = lstatSync(current); }
    catch (error) { if (error.code === "ENOENT") continue; throw error; }
    if (entry.isSymbolicLink() || !entry.isDirectory()) {
      throw new Error("parallel FULL Cargo output requires plain directory ancestors");
    }
    const canonical = realpathSync.native(current);
    if (!contained(canonical)) throw new Error("parallel FULL Cargo output escapes its worktree");
    current = canonical;
  }
  if (current === canonicalRoot) throw new Error("parallel FULL Cargo output cannot be the worktree root");
  return current;
}

export function isolatedStageEnvironments(root, runId, env, cargoTarget) {
  if (!/^test-[a-zA-Z0-9-]+$/u.test(runId)) throw new Error("invalid isolation run id");
  root = resolve(root);
  const originalHome = env.HOME;
  if (!originalHome || !isAbsolute(originalHome)) throw new Error("parallel FULL needs an absolute toolchain HOME");
  const cargoHome = env.CARGO_HOME ?? join(originalHome, ".cargo");
  const rustupHome = env.RUSTUP_HOME ?? join(originalHome, ".rustup");
  const target = validateCargoOutputTarget(root,
    cargoTarget ?? env.CARGO_TARGET_DIR ?? env.CARGO_BUILD_TARGET_DIR ?? "target");
  if (!isAbsolute(cargoHome) || !isAbsolute(rustupHome)) {
    throw new Error("parallel FULL needs private in-worktree Cargo output and absolute toolchain cache paths");
  }
  const directory = plainDirectory(join(plainDirectory(join(root, ".tmp")), `parallel-full-${runId}`));
  const environments = {}, paths = {};
  for (const name of ["rust-tests", "vitest"]) {
    const stageRoot = plainDirectory(join(directory, name));
    const home = plainDirectory(join(stageRoot, "home"));
    const temp = plainDirectory(join(stageRoot, "temp"));
    const engramHome = plainDirectory(join(stageRoot, "engram"));
    const config = join(home, ".gitconfig");
    writeFileSync(config, "", { flag: "wx", mode: 0o600 });
    const child = { ...isolatedGitEnvironment(env), HOME: home,
      ENGRAM_HOME: engramHome, GIT_CONFIG_GLOBAL: config,
      CARGO_HOME: cargoHome, RUSTUP_HOME: rustupHome,
      TERMAL_TEST_USER_TEMP: temp, TEMP: temp, TMP: temp, TMPDIR: temp,
      NODE_COMPILE_CACHE: plainDirectory(join(stageRoot, "node-compile-cache")),
    };
    delete child.TERMAL_TEST_RUN_ROOT;
    for (const [key, childName] of Object.entries({
      XDG_CONFIG_HOME: "config", XDG_CACHE_HOME: "cache", XDG_DATA_HOME: "data", XDG_RUNTIME_DIR: "runtime",
    })) child[key] = plainDirectory(join(stageRoot, childName));
    environments[name] = Object.freeze(child);
    paths[name] = { root: stageRoot, home, temp, engramHome, cargoTarget: target };
  }
  return { directory, environments, paths };
}

export function removeStageIsolation(root, directory) {
  const expectedParent = resolve(root, ".tmp");
  const relativePath = relative(expectedParent, directory);
  if (!/^parallel-full-test-[a-zA-Z0-9-]+$/u.test(relativePath)) {
    throw new Error("refused isolation cleanup outside the run's private temporary root");
  }
  // Do not traverse a replaced parent or root. rm does not follow child links.
  for (const path of [expectedParent, directory]) {
    const entry = lstatSync(path);
    if (entry.isSymbolicLink() || !entry.isDirectory()) throw new Error(`expected plain directory: ${path}`);
  }
  rmSync(directory, { recursive: true, force: false });
}
