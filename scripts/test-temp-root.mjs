// Owns test-process temporary storage, crash leftovers and escape detection.
// Does not delete historical artifacts outside the product directory or retry
// failed removals. Shared by the Rust launcher and Node fixture tests.
import {
  lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync,
  rmSync, writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";

const markerName = ".termal-test-run";
const productName = "termal";
const ownedOutsidePrefixes = ["termal-"];
const staleAgeMs = 48 * 60 * 60 * 1000;
const maxSweepRemovals = 64;

function directoryWithoutLinks(path) {
  try {
    mkdirSync(path);
  } catch (error) {
    if (error.code !== "EEXIST") throw error;
  }
  const metadata = lstatSync(path);
  if (metadata.isSymbolicLink() || !metadata.isDirectory()) {
    throw new Error(`Test temporary directory is not a plain directory (link?): ${path}`);
  }
  return path;
}

function productRoot(userTemp) {
  if (!isAbsolute(userTemp)) throw new Error(`User temp path must be absolute: ${userTemp}`);
  if (hasParentTraversal(userTemp)) throw new Error(`User temp path must not contain parent traversal: ${userTemp}`);
  // Match resolve_test_temp_directory in Rust: normalize lexical dot/separator
  // spelling, not filesystem aliases. The OS-selected user-temp ancestors may
  // be aliases (e.g. macOS /var); every product/run component rejects links.
  return directoryWithoutLinks(join(directoryWithoutLinks(join(resolve(userTemp), productName)), "tests"));
}

function hasParentTraversal(path) {
  return path.split(process.platform === "win32" ? /[\\/]/ : /\//).includes("..");
}

export function testTempDirectory() {
  const root = productRoot(process.env.TERMAL_TEST_USER_TEMP ?? tmpdir());
  const runRoot = process.env.TERMAL_TEST_RUN_ROOT;
  if (runRoot === undefined) return root;
  if (!isAbsolute(runRoot) || hasParentTraversal(runRoot) ||
      dirname(resolve(runRoot)) !== root || !basename(resolve(runRoot)).startsWith("run-")) {
    throw new Error(`Test run root must be a direct run-* child of ${root} without parent traversal: ${runRoot}`);
  }
  return directoryWithoutLinks(resolve(runRoot));
}

function validProcessId(pid) {
  return Number.isSafeInteger(pid) && pid > 0;
}

function processMayBeAlive(pid) {
  if (!validProcessId(pid)) return true;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    // Permission failures are not proof of process death.
    return error.code !== "ESRCH";
  }
}

function removalFailure(label, path, error) {
  let survivors;
  try {
    const metadata = lstatSync(path);
    if (metadata.isDirectory() && !metadata.isSymbolicLink()) {
      const entries = readdirSync(path);
      survivors = entries.slice(0, 64).map((name) => join(path, name)).join(", ") || "(empty)";
      if (entries.length > 64) survivors += `; ${entries.length - 64} more entries`;
    } else {
      survivors = `${path} (not a plain directory)`;
    }
  } catch (listingError) {
    survivors = `unable to list ${path}: ${listingError.message}`;
  }
  return new Error(`${label} ${path}: ${error.message}; code=${error.code ?? "unknown"}; errno=${error.errno ?? "unknown"}; surviving entries: ${survivors}`, { cause: error });
}

function lstatIfPresent(path) {
  try {
    return lstatSync(path);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    return undefined; // A concurrent owner already removed this entry.
  }
}

function existingPlainDirectory(path) {
  const metadata = lstatIfPresent(path);
  if (!metadata) return false;
  if (metadata.isSymbolicLink() || !metadata.isDirectory()) {
    throw new Error(`Test temporary directory is not a plain directory (link?): ${path}`);
  }
  return true;
}

function staleEntry(path, recognizeRuns) {
  const fixture = lstatIfPresent(path);
  if (!fixture || fixture.isSymbolicLink() ||
      (!fixture.isDirectory() && !fixture.isFile())) return undefined;
  const marker = join(path, markerName);
  const metadata = recognizeRuns && fixture.isDirectory() ? lstatIfPresent(marker) : undefined;
  if (metadata) {
    if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size > 1024) return undefined;
    if (Date.now() - metadata.mtimeMs < staleAgeMs) return undefined;
    let owner;
    try {
      owner = JSON.parse(readFileSync(marker, "utf8"));
    } catch {
      return undefined; // An unreadable marker cannot prove the run is dead.
    }
    if (!owner || typeof owner !== "object" || Array.isArray(owner) ||
        owner.version !== 1 || !validProcessId(owner.pid) ||
        ("childPid" in owner && !validProcessId(owner.childPid))) return undefined;
    if (processMayBeAlive(owner.pid)) return undefined;
    if (owner.childPid !== undefined && processMayBeAlive(owner.childPid)) return undefined;
  } else if (Date.now() - fixture.mtimeMs < staleAgeMs) {
    return undefined;
  }
  return fixture;
}

function sweepStaleEntries(root, recognizeRuns) {
  if (!existingPlainDirectory(root)) return [];
  const removed = [];
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    if (removed.length === maxSweepRemovals) break;
    if (entry.isSymbolicLink() || (!entry.isDirectory() && !entry.isFile())) continue;
    const path = join(root, entry.name);
    const fixture = staleEntry(path, recognizeRuns);
    if (!fixture) continue;
    // Read-only revalidation: never recreate a concurrently removed target.
    // Recheck age/marker/owner as well as type, and skip replaced entries.
    if (!existingPlainDirectory(root)) break;
    const current = staleEntry(path, recognizeRuns);
    if (!current || current.dev !== fixture.dev || current.ino !== fixture.ino ||
        current.isDirectory() !== fixture.isDirectory()) continue;
    try {
      rmSync(path, { recursive: current.isDirectory(), maxRetries: 0 });
    } catch (error) {
      if (error.code === "ENOENT") continue;
      throw removalFailure("Failed to remove stale temporary entry", path, error);
    }
    removed.push(path);
  }
  return removed;
}

export function sweepStaleTestRuns(root) {
  // Direct fixtures belong to the product by location, not spelling. Files and
  // directories share one cap; never select nested fixtures inside a recent run.
  return sweepStaleEntries(root, true);
}

function outsideEntries(userTemp, prefixes = ownedOutsidePrefixes) {
  return readdirSync(userTemp).filter((name) => prefixes.some((prefix) => name.toLowerCase().startsWith(prefix)));
}

export async function runInTestTemp(command, args, {
  userTemp = process.env.TERMAL_TEST_USER_TEMP ?? tmpdir(),
  report = console.log,
} = {}) {
  const root = productRoot(userTemp);
  userTemp = dirname(dirname(root));
  const compileCache = directoryWithoutLinks(join(dirname(root), "node-compile-cache"));
  // Persist recent cache data across launches; evict up to 64 direct entries
  // older than 48 h per launch, without following links or deleting the root.
  const cacheSwept = sweepStaleEntries(compileCache, false);
  if (cacheSwept.length) report(`Removed ${cacheSwept.length} stale Node cache entries under ${compileCache}`);
  const before = outsideEntries(userTemp);
  const otherBefore = outsideEntries(userTemp, ["codenav-"]);
  const swept = sweepStaleTestRuns(root);
  if (swept.length) report(`Removed ${swept.length} stale test run(s) under ${root}`);
  const runRoot = mkdtempSync(join(root, "run-"));
  const marker = join(runRoot, markerName);
  writeFileSync(marker, JSON.stringify({ version: 1, pid: process.pid }));
  report(`Test temp run: ${runRoot}; outside product root before=${before.length}`);
  let exitCode;
  let spawnError;
  try {
    exitCode = await new Promise((resolveExit, reject) => {
      const child = spawn(command, args, {
        stdio: "inherit",
        windowsHide: true,
        env: {
          ...process.env,
          TERMAL_TEST_USER_TEMP: userTemp,
          TERMAL_TEST_RUN_ROOT: runRoot,
          NODE_COMPILE_CACHE: compileCache,
          TEMP: runRoot,
          TMP: runRoot,
          TMPDIR: runRoot,
        },
      });
      child.once("error", reject);
      child.once("spawn", () => {
        writeFileSync(marker, JSON.stringify({ version: 1, pid: process.pid, childPid: child.pid }));
      });
      child.once("close", (code) => resolveExit(code ?? 1));
    });
  } catch (error) {
    spawnError = new Error(`Test executable ${command} failed; retained ${runRoot}: ${error.message}`, { cause: error });
    exitCode = 1;
  }
  const after = outsideEntries(userTemp);
  const otherAfter = outsideEntries(userTemp, ["codenav-"]);
  const escaped = after.filter((entry) => !before.includes(entry));
  report(`Test temp guard: outside product root before=${before.length}, after=${after.length}, new=${escaped.length}`);
  report(`Other product (informational): codenav- before=${otherBefore.length}, after=${otherAfter.length}, delta=${otherAfter.length - otherBefore.length}`);
  if (escaped.length > 0) {
    report(`Test temporary artifacts escaped containment: ${escaped.join(", ")}`);
    if (exitCode === 0) exitCode = 1;
  }
  if (exitCode === 0) {
    productRoot(userTemp);
    directoryWithoutLinks(runRoot);
    const residual = readdirSync(runRoot).filter((name) => name !== markerName);
    report(`Test temp cleanup: ${residual.length} remaining entries in ${runRoot}: ${residual.map((name) => JSON.stringify(name)).join(", ") || "(empty)"}`);
    try {
      rmSync(runRoot, { recursive: true, maxRetries: 0 });
    } catch (error) {
      throw removalFailure("Failed to remove green test run", runRoot, error);
    }
  } else {
    report(`Retained failed test run: ${runRoot}`);
  }
  if (spawnError) throw spawnError;
  return { exitCode, runRoot, beforeCount: before.length, afterCount: after.length };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [command, ...args] = process.argv.slice(2);
  if (!command) {
    console.error("Usage: node scripts/test-temp-root.mjs <executable> [arguments...]");
    process.exitCode = 2;
  } else {
    try {
      process.exitCode = (await runInTestTemp(command, args)).exitCode;
    } catch (error) {
      console.error(error.message);
      process.exitCode = 1;
    }
  }
}
