// Read-only visibility for the coordinator's ordinary status sweep. This does
// not decide removal eligibility, walk directory contents or open a work store.
import fs from "node:fs/promises";
import { realpathSync } from "node:fs";
import path from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { isolatedGitEnvironment } from "./review-freeze-fingerprint.mjs";

export function parseWorktrees(text) {
  return text.split("\0\0").filter(Boolean).map(block => {
    const fields = Object.fromEntries(block.split("\0").filter(Boolean).map(line => {
      const at = line.indexOf(" ");
      return at < 0 ? [line, true] : [line.slice(0, at), line.slice(at + 1)];
    }));
    if (typeof fields.worktree !== "string" || typeof fields.HEAD !== "string") {
      throw new Error("Incomplete Git worktree record");
    }
    return fields;
  });
}

function key(value) {
  const plain = value.replace(/^\\\\\?\\UNC\\/i, "\\\\").replace(/^\\\\\?\\/, "");
  // Preserve UNC identity even on a host whose path.resolve treats it as relative.
  if (plain.startsWith("\\\\")) return path.win32.normalize(plain).toLowerCase();
  const resolved = path.resolve(plain);
  return process.platform === "win32" ? resolved.toLowerCase() : resolved;
}

export function contains(parent, child) {
  const from = key(parent), to = key(child);
  return from === to || to.startsWith(from.endsWith(path.sep) ? from : from + path.sep);
}

export async function readNamedRoots(baseUrl, fetcher = fetch) {
  if (!baseUrl) return { state: "unknown", reason: "TERMAL_BASE_URL is absent", roots: [] };
  try {
    const url = new URL(baseUrl);
    if (!["http:", "https:"].includes(url.protocol) || url.username || url.password) throw new Error();
    const response = await fetcher(new URL("/api/engram/named-roots", url), { signal: AbortSignal.timeout(3000) });
    if (!response.ok) throw new Error();
    const roots = await response.json();
    if (!Array.isArray(roots) || !roots.every(r => r && typeof r.root === "string" && path.isAbsolute(r.root) && typeof r.shortRef === "string" && r.shortRef.trim())) throw new Error();
    return { state: "known", roots: roots.map(r => ({ root: r.root, workRef: r.shortRef })) };
  } catch {
    return { state: "unknown", reason: "named-root API unavailable or invalid", roots: [] };
  }
}

export function validateRetentions(value, repository) {
  if (!value || !Number.isFinite(Date.parse(value.observedAt)) || !Array.isArray(value.entries)) {
    throw new Error("Retentions require observedAt and an entries array");
  }
  return { observedAt: value.observedAt, entries: value.entries.map(entry => {
    if (!entry || !["path", "owner", "reason", "until"].every(k => typeof entry[k] === "string" && entry[k].trim())) {
      throw new Error("Each retention needs path, owner, reason and until");
    }
    if (/^\\\\(?:\?\\UNC\\|[^?])/i.test(entry.path)) throw new Error("Retention path is outside the local repository");
    const location = path.resolve(repository, entry.path);
    if (!contains(repository, location)) throw new Error("Retention path is outside the repository");
    return { path: location, owner: entry.owner, reason: entry.reason, until: entry.until };
  }) };
}

export async function collectReport({ repository, worktrees, namedRoots, retentions = null, filesystem = fs }) {
  const root = path.resolve(repository);
  const rows = new Map();
  const warnings = [];
  const add = (location, kind, git = null) => {
    const id = key(location);
    if (!rows.has(id)) rows.set(id, { path: path.resolve(location), kinds: [], git: null });
    const row = rows.get(id);
    if (!row.kinds.includes(kind)) row.kinds.push(kind);
    if (git) row.git = { head: git.HEAD, branch: git.branch ?? null, dirty: "unknown", merged: "unknown" };
  };
  for (const tree of worktrees) add(tree.worktree, "worktree", tree);
  // lstat every group before readdir: a linked .tmp/.logs/.evidence is never traversed.
  for (const group of [".tmp", ".logs", ".evidence"]) {
    const location = path.join(root, group);
    try {
      const metadata = await filesystem.lstat(location);
      if (metadata.isSymbolicLink() || !metadata.isDirectory()) {
        add(location, group);
        warnings.push(`${group}: not a plain directory; contents not listed`);
        continue;
      }
      for (const name of await filesystem.readdir(location)) add(path.join(location, name), group);
    } catch (error) {
      if (error.code !== "ENOENT") warnings.push(`${group}: listing unavailable (${error.code ?? "error"})`);
    }
  }
  for (const row of rows.values()) {
    row.owner = "unknown";
    row.ownerGuess = path.basename(row.path).match(/[-_](\d{4,})$/)?.[1] ?? null;
    row.processUse = "unknown";
    row.openItemReferences = "unknown";
    row.runEvidence = "unknown";
    row.logicalBytes = null;
    row.classification = "review";
    row.reason = "ownership, live use and retention require review; absence is not disposal proof";
    const isMain = key(row.path) === key(root);
    row.retentions = retentions ? (isMain ? [] : retentions.entries.filter(r => contains(row.path, r.path) || contains(r.path, row.path))) : null;
    row.namedRoots = namedRoots.state === "known" ? namedRoots.roots.filter(r => contains(row.path, r.root)) : null;
    if (isMain || row.namedRoots?.length || row.retentions?.length) {
      row.classification = "retain";
      row.reason = isMain ? "main checkout" : row.namedRoots?.length ? "contains a current named root" : "reported retention; recheck that its need remains active";
      if (!isMain && row.retentions?.length === 1 && contains(row.retentions[0].path, row.path)) row.owner = row.retentions[0].owner;
    }
    if (!contains(root, row.path)) {
      row.type = "outside-repository";
      row.reason += "; external worktree listed by Git; filesystem not inspected";
      continue;
    }
    try {
      const metadata = await filesystem.lstat(row.path);
      row.type = metadata.isSymbolicLink() ? "link" : metadata.isFile() ? "file" : metadata.isDirectory() ? "directory" : "other";
      if (row.type === "file") row.logicalBytes = metadata.size;
      row.modifiedAt = metadata.mtime.toISOString();
    } catch (error) {
      row.type = error.code === "ENOENT" ? "absent" : "unreadable";
      row.reason += `; metadata unavailable (${error.code ?? "error"})`;
    }
  }
  const entries = [...rows.values()].sort((a, b) => a.path.localeCompare(b.path));
  return {
    observedAt: new Date().toISOString(), repository: root,
    namedRoots: { state: namedRoots.state, reason: namedRoots.reason ?? null },
    retentionSnapshot: retentions?.observedAt ?? "not supplied",
    unmatchedRetentions: retentions?.entries.filter(r => !entries.some(row => row.retentions?.includes(r))) ?? null,
    warnings,
    counts: { total: entries.length, retain: entries.filter(r => r.classification === "retain").length, review: entries.filter(r => r.classification === "review").length },
    limitations: ["Read-only visibility, not removal authority.", "Directories are not walked: their sizes remain unknown.", "No dirty-state, reachability, process, run or tracker-reference census is performed.", "Owner guesses and retention snapshots are not current execution authority."],
    entries,
  };
}

export function parseOptions(args) {
  const options = { json: false, limit: 30, retentions: null };
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--json") options.json = true;
    else if (args[i] === "--retentions" && args[i + 1]) options.retentions = args[++i];
    else if (args[i] === "--limit" && /^\d+$/.test(args[i + 1] ?? "")) options.limit = Number(args[++i]);
    else throw new Error("Usage: node scripts/cleanup-report.mjs [--json] [--limit 1..500] [--retentions FILE.json]");
  }
  if (options.limit < 1 || options.limit > 500) throw new Error("Limit must be 1..500");
  return options;
}

export function shapeReport(report, options) {
  if (options.json) return report;
  const entries = [...report.entries].sort((a, b) => Number(a.classification === "retain") - Number(b.classification === "retain") || a.path.localeCompare(b.path));
  return { ...report, omitted: Math.max(0, entries.length - options.limit), entries: entries.slice(0, options.limit) };
}

export async function readRetentions(file, repository) {
  const location = path.resolve(repository, file);
  if (!contains(repository, location)) throw new Error("Retentions snapshot is outside the repository");
  if (path.extname(location).toLowerCase() !== ".json") throw new Error("Retentions must be a JSON snapshot, not a store");
  const metadata = await fs.lstat(location);
  if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size > 8 * 1024 * 1024) throw new Error("Retentions must be a plain JSON file up to 8 MiB");
  if (!contains(await fs.realpath(repository), await fs.realpath(location))) throw new Error("Retentions snapshot resolves outside the repository");
  return validateRetentions(JSON.parse(await fs.readFile(location, "utf8")), repository);
}

async function main() {
  const options = parseOptions(process.argv.slice(2));
  const git = async args => (await promisify(execFile)("git", args, {
    cwd: process.cwd(), env: isolatedGitEnvironment(), windowsHide: true,
    encoding: "utf8", timeout: 10000, maxBuffer: 8 * 1024 * 1024,
  })).stdout;
  const common = (await git(["rev-parse", "--path-format=absolute", "--git-common-dir"])).trim();
  if (path.basename(common) !== ".git") throw new Error("Expected the repository's .git common directory");
  const repository = path.dirname(common);
  const worktrees = parseWorktrees(await git(["worktree", "list", "--porcelain", "-z"]));
  const retentions = options.retentions ? await readRetentions(options.retentions, repository) : null;
  const report = await collectReport({ repository, worktrees, retentions, namedRoots: await readNamedRoots(process.env.TERMAL_BASE_URL) });
  console.log(JSON.stringify(shapeReport(report, options), null, 2));
}

function invokedDirectly() {
  if (!process.argv[1]) return false;
  try { return realpathSync.native(path.resolve(process.argv[1])) === realpathSync.native(fileURLToPath(import.meta.url)); }
  catch { return false; }
}

if (invokedDirectly()) {
  main().catch(error => { console.error(`cleanup report failed: ${error.message}`); process.exitCode = 1; });
}
