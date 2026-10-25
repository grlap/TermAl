// Full-gate UI dependency setup, split from test-launcher.mjs.
// Owns locked private installation and setup evidence, not gate stages,
// registration, notifications, host configuration or global tool installation.
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { lstatSync, readFileSync, realpathSync, statSync } from "node:fs";
import { basename, delimiter, dirname, isAbsolute, join, relative } from "node:path";
import { isDeepStrictEqual } from "node:util";
import { isolatedGitEnvironment } from "./review-freeze-fingerprint.mjs";

export const uiDependencyFiles = Object.freeze([
  "node_modules/typescript/bin/tsc",
  "node_modules/vitest/vitest.mjs",
]);

// Invoke npm's JavaScript entry with this launcher's Node, without a shell or
// a Windows .cmd trampoline. Standard Node distributions and PATH symlinks
// cover Windows, macOS, Linux and package-manager Node installations.
export function resolveNpmCli(env = process.env, node = process.execPath) {
  const searchPath = Object.entries(env).find(([key]) => key.toLowerCase() === "path")?.[1] ?? "";
  const directories = [dirname(realpathSync(node)), ...searchPath.split(delimiter).filter(Boolean)];
  const candidates = [
    ...(env.npm_execpath ? [env.npm_execpath] : []),
    ...directories.flatMap((directory) => [
      join(directory, "node_modules/npm/bin/npm-cli.js"),
      join(directory, "../lib/node_modules/npm/bin/npm-cli.js"),
      join(directory, "npm"),
    ]),
  ];
  for (const candidate of candidates) {
    try {
      const path = realpathSync(candidate);
      if (basename(path) === "npm-cli.js" && statSync(path).isFile()) return path;
    } catch {
      // A missing PATH entry is not permission to install a global tool.
    }
  }
  throw new Error("npm CLI is unavailable; the Node/npm toolchain must provide npm-cli.js");
}

function git(root, env, args) {
  const result = spawnSync("git", args, {
    cwd: root, env: isolatedGitEnvironment(env), encoding: "utf8", windowsHide: true,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`UI dependency setup requires ${args.join(" ")}`);
}

function validatePrivateUi(root, env) {
  const ui = join(root, "ui");
  const fromRoot = relative(realpathSync(root), realpathSync(ui));
  if (!fromRoot || fromRoot === ".." || fromRoot.startsWith(`..${process.platform === "win32" ? "\\" : "/"}`) || isAbsolute(fromRoot)) {
    throw new Error("UI directory must resolve inside the candidate worktree");
  }
  const modules = join(ui, "node_modules");
  try {
    if (lstatSync(modules).isSymbolicLink()) {
      throw new Error("UI dependencies must be private; node_modules cannot be a symlink or junction");
    }
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  git(root, env, ["ls-files", "--error-unmatch", "--", "ui/package.json", "ui/package-lock.json"]);
  git(root, env, ["check-ignore", "--quiet", "--", "ui/node_modules/"]);
  for (const name of ["package.json", "package-lock.json"]) {
    if (!lstatSync(join(ui, name)).isFile()) throw new Error(`UI ${name} must be a regular file`);
  }
  return ui;
}

// Never run npm ci over an existing tree: another worktree may link INTO it.
// npm's installed lock plus actual package manifests must agree with the source
// lock before reuse. A stale/partial tree fails setup without deleting anything.
function verifyInstalledUi(ui) {
  const read = (path) => JSON.parse(readFileSync(path, "utf8"));
  const lock = read(join(ui, "package-lock.json"));
  const manifest = read(join(ui, "package.json"));
  const installed = read(join(ui, "node_modules/.package-lock.json"));
  if (!lock.packages || !installed.packages) throw new Error("installed UI dependencies need npm lockfile metadata");
  for (const key of ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"]) {
    if (!isDeepStrictEqual(manifest[key] ?? {}, lock.packages[""]?.[key] ?? {})) {
      throw new Error(`UI package.json ${key} does not match package-lock.json`);
    }
  }
  for (const [name, entry] of Object.entries(lock.packages)) {
    if (!name) continue;
    if (!Object.hasOwn(installed.packages, name) && entry.optional) continue;
    if (!isDeepStrictEqual(entry, installed.packages[name])) throw new Error(`installed UI lock differs at ${name}`);
  }
  for (const [name, entry] of Object.entries(installed.packages)) {
    if (!name.startsWith("node_modules/") || name.split(/[\\/]/u).includes("..") ||
        !isDeepStrictEqual(entry, lock.packages[name]) || entry.link) {
      throw new Error(`installed UI package is outside the source lock: ${name}`);
    }
    const path = join(ui, name);
    if (!lstatSync(path).isDirectory() || read(join(path, "package.json")).version !== entry.version) {
      throw new Error(`installed UI package does not match its lock: ${name}`);
    }
  }
}

export async function bootstrapUiDependencies(root, {
  env = process.env, log, run, observe = () => {}, npmCli = resolveNpmCli,
} = {}) {
  const entry = { name: "ui-dependencies", state: "running", started: new Date().toISOString(), log };
  observe(entry);
  try {
    entry.phase = "validate-lockfile";
    entry.cwd = validatePrivateUi(root, env);
    entry.lockfile = join(entry.cwd, "package-lock.json");
    entry.lockSha256 = createHash("sha256").update(readFileSync(entry.lockfile)).digest("hex");
    let existing;
    try { existing = lstatSync(join(entry.cwd, "node_modules")); }
    catch (error) { if (error.code !== "ENOENT") throw error; }
    if (existing) {
      entry.phase = "verify-installed";
      verifyInstalledUi(entry.cwd);
      entry.action = "reused";
      entry.code = 0;
    } else {
      entry.phase = "resolve-npm";
      const cli = npmCli(env);
      entry.command = [process.execPath, cli, "ci", "--include=dev", "--no-audit", "--no-fund"];
      entry.phase = "npm-ci";
      entry.action = "installed";
      observe(entry);
      Object.assign(entry, await run(entry.command[0], entry.command.slice(1), {
        cwd: entry.cwd, env, log,
      }));
      if (entry.interrupted) throw Object.assign(new Error("UI dependency setup interrupted"), { interrupted: true });
      if (entry.code !== 0 || entry.error) throw new Error(entry.error ?? `npm ci exited ${entry.code}`);
    }
    entry.phase = "verify-entrypoints";
    for (const name of uiDependencyFiles) {
      if (!statSync(join(entry.cwd, name)).isFile()) throw new Error(`UI dependency is not a file: ${name}`);
    }
    if (createHash("sha256").update(readFileSync(entry.lockfile)).digest("hex") !== entry.lockSha256) {
      throw new Error("npm ci changed the candidate lockfile");
    }
    entry.state = "passed";
    entry.ended = new Date().toISOString();
    observe(entry);
    return entry;
  } catch (cause) {
    entry.state = "failed";
    entry.error = cause.message;
    entry.ended = new Date().toISOString();
    observe(entry);
    if (cause.interrupted) throw cause;
    throw Object.assign(new Error(`UI dependency setup failed (${entry.phase ?? "initialize"}): ${cause.message}`), {
      setupFailure: { kind: "ui-dependencies", phase: entry.phase ?? "initialize", log },
    });
  }
}
