// Automatic launcher source input uses the producer-owned source contract.
// Owns closed value comparison and compact run-record fit/refusal helpers;
// source overflow never trims an observation or replaces actual stage facts.
// Explicit review-freeze identity stays in review-freeze-fingerprint.mjs.
import { createHash } from "node:crypto";
import { readFileSync, realpathSync } from "node:fs";
import { isAbsolute, resolve } from "node:path";
import { homedir } from "node:os";
import { execFile } from "node:child_process";
import { sourceProcessBudgetMs, warnSlowEngramOperation } from "./source-id-budget.mjs";

// Binary selection is setup, not another source predicate. Never search PATH or
// build an owner during admission. Tests explicitly select their current build.
export function sourceBinaryPath(env = process.env) {
  if (Object.hasOwn(env, "ENGRAM_SOURCE_BINARY")) {
    if (typeof env.ENGRAM_SOURCE_BINARY !== "string" || !isAbsolute(env.ENGRAM_SOURCE_BINARY)) {
      throw new Error("ENGRAM_SOURCE_BINARY must name an absolute engram executable");
    }
    return resolve(env.ENGRAM_SOURCE_BINARY);
  }
  const cargoHome = Object.hasOwn(env, "CARGO_HOME") ? env.CARGO_HOME : resolve(homedir(), ".cargo");
  if (typeof cargoHome !== "string" || !isAbsolute(cargoHome)) {
    throw new Error("CARGO_HOME must be absolute, or set ENGRAM_SOURCE_BINARY explicitly");
  }
  return resolve(cargoHome, "bin", process.platform === "win32" ? "engram.exe" : "engram");
}

const SOURCE_JSON_BYTES = 1024 * 1024;
// The host reads each ordinary run artifact under this existing byte cap.
// Count the exact UTF-8 wire image, including the writer's final newline.
export const RUN_RECORD_JSON_BYTES = 1024 * 1024;
export function encodeRunRecord(value, artifact) {
  const bytes = Buffer.from(`${JSON.stringify(value)}\n`, "utf8");
  if (bytes.length > RUN_RECORD_JSON_BYTES) {
    throw Object.assign(new Error(`${artifact}: output_limit (${bytes.length} UTF-8 bytes exceeds ${RUN_RECORD_JSON_BYTES})`),
      { code: "RUN_RECORD_OUTPUT_LIMIT", artifact });
  }
  return bytes;
}

// A complete observation either fits or becomes unavailable. Stage objects
// and their original outcomes are never shortened to make a source fit.
export function refuseRunSource(result, artifact) {
  const reason = `${artifact}: output_limit; complete source_id observation cannot fit the 1048576-byte run artifact`;
  result.sourceIdAfter = null;
  result.sourceUnavailableReason = reason;
  result.sourceInputReason = "input-unverified";
  result.reason ??= "input-unverified";
  result.error ??= reason;
  if (result.state !== "failed") {
    result.state = "failed";
    result.exitCode = result.stages?.find((stage) => stage.state === "failed")?.code || 1;
  }
  if (result.input?.kind === "commit") {
    result.input.eligible = false;
    result.input.unavailableReason = reason;
    delete result.input.after;
    delete result.input.outcome;
  }
}

export function encodeTerminalRunResult(result) {
  try { return encodeRunRecord(result, "results.json"); }
  catch (error) {
    if (error.code !== "RUN_RECORD_OUTPUT_LIMIT"
        || !(result.sourceIdBefore || result.sourceIdAfter)) throw error;
  }
  refuseRunSource(result, "results.json");
  try { return encodeRunRecord(result, "results.json"); }
  catch (error) {
    if (error.code !== "RUN_RECORD_OUTPUT_LIMIT") throw error;
  }
  result.sourceIdBefore = null;
  return encodeRunRecord(result, "results.json");
}
const sourceCodes = new Set(["input_unavailable", "deadline", "output_limit"]);
const fieldsExactly = (value, fields) => value !== null && typeof value === "object" && !Array.isArray(value)
  && Object.keys(value).sort().join("\0") === [...fields].sort().join("\0");
const sourceText = (value) => typeof value === "string" && value.length > 0 && !value.includes("\0")
  && Buffer.from(value).toString("utf8") === value;
const sourceOid = (value) => typeof value === "string" && /^[a-f0-9]{40}$/u.test(value);
const sourcePath = (value) => sourceText(value) && !isAbsolute(value) && !/[\r\n]/u.test(value)
  && value.split("/").every((part) => part !== "" && part !== "." && part !== "..");
const byteOrder = (a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b));
const changeOrder = (a, b) => byteOrder(a.path, b.path) || byteOrder(a.status, b.status)
  || byteOrder(a.origin ?? "", b.origin ?? "");

export function sourceOwnerPinValid(pin) {
  return fieldsExactly(pin, ["binaryPath", "sha256", "binaryVersion", "sourceVersion"])
    && typeof pin.binaryPath === "string" && isAbsolute(pin.binaryPath)
    && Buffer.byteLength(pin.binaryPath) <= 4096 && !/\p{Cc}/u.test(pin.binaryPath)
    && typeof pin.sha256 === "string" && /^[a-f0-9]{64}$/u.test(pin.sha256)
    && typeof pin.binaryVersion === "string" && pin.binaryVersion.startsWith("engram ")
    && pin.binaryVersion.length > 7 && Buffer.byteLength(pin.binaryVersion) <= 512
    && /^[\x20-\x7e]+$/u.test(pin.binaryVersion) && pin.sourceVersion === 1;
}

function verifySourceOwnerPin(pin) {
  if (!sourceOwnerPinValid(pin)) throw new Error("source owner pin is malformed or unsupported");
  const binary = realpathSync.native(pin.binaryPath);
  const digest = createHash("sha256").update(readFileSync(binary)).digest("hex");
  if (digest !== pin.sha256) throw new Error("source owner executable no longer matches its pin");
  return binary;
}

function sourceChanges(changes) {
  if (!Array.isArray(changes)) return null;
  const result = [];
  for (const change of changes) {
    if (!fieldsExactly(change, ["path", "status", "origin", "blob", "mode"])
        || !sourcePath(change.path) || typeof change.status !== "string"
        || !/^[ MADRCUT?]{2}$/u.test(change.status) || change.status === "  "
        || (/[RC]/u.test(change.status) ? !sourcePath(change.origin) : change.origin !== null)
        || (change.blob === null ? change.mode !== null || !change.status.includes("D")
          : !sourceOid(change.blob) || !["100644", "100755", "120000"].includes(change.mode))) return null;
    if (result.length && changeOrder(result.at(-1), change) >= 0) return null;
    result.push({ path: change.path, status: change.status, origin: change.origin, blob: change.blob, mode: change.mode });
  }
  return result;
}

// Rebuild only object key order. Array/path order is validated, never repaired.
// This compares the owner's complete value without introducing another hash.
function sourceValue(value) {
  try { if (Buffer.byteLength(JSON.stringify(value)) > SOURCE_JSON_BYTES) return null; }
  catch { return null; }
  if (!fieldsExactly(value, ["commit", "tree", "submodules", "changes"])
      || !sourceOid(value.commit) || !sourceOid(value.tree) || !Array.isArray(value.submodules)) return null;
  const changes = sourceChanges(value.changes);
  if (changes === null) return null;
  const submodules = [];
  for (const child of value.submodules) {
    if (!fieldsExactly(child, ["path", "commit", "changes"]) || !sourcePath(child.path) || !sourceOid(child.commit)
        || (submodules.length && byteOrder(submodules.at(-1).path, child.path) >= 0)) return null;
    const local = sourceChanges(child.changes);
    if (local === null) return null;
    submodules.push({ path: child.path, commit: child.commit, changes: local });
  }
  return { commit: value.commit, tree: value.tree, submodules, changes };
}

export function sourceIdsEqual(before, after) {
  const a = sourceValue(before), b = sourceValue(after);
  return a !== null && b !== null && JSON.stringify(a) === JSON.stringify(b);
}

export const sourceIdLimitations = () => [
  "Source identity limitation: ordinary Git normalization and ignored-path policy apply; boundary equality does not prove the absence of transient edits or provide raw-byte coverage.",
];

// Investigation context from the already captured values; never query Git or
// emit the potentially large complete identities into a diagnostic.
export function sourceIdDifference(before, after) {
  const a = sourceValue(before), b = sourceValue(after);
  if (!a || !b) return "source identity unavailable or malformed; inspect results.json";
  const lines = [];
  const add = (text) => { if (lines.length < 12) lines.push(text); };
  if (a.commit !== b.commit) add(`root commit: ${a.commit} -> ${b.commit}`);
  if (a.tree !== b.tree) add(`root tree: ${a.tree} -> ${b.tree}`);
  const repositories = [["", a.changes, b.changes]];
  const children = new Map(b.submodules.map((child) => [child.path, child]));
  const label = (path) => JSON.stringify(path.slice(0, 160)) + (path.length > 160 ? "…" : "");
  for (const child of a.submodules) {
    const later = children.get(child.path);
    if (!later) add(`child removed: ${label(child.path)}`);
    else {
      if (child.commit !== later.commit) add(`child ${label(child.path)} commit: ${child.commit} -> ${later.commit}`);
      repositories.push([`${child.path}/`, child.changes, later.changes]);
      children.delete(child.path);
    }
  }
  for (const child of children.values()) add(`child added: ${label(child.path)} at ${child.commit}`);
  const key = (entry) => JSON.stringify([entry.path, entry.status, entry.origin]);
  for (const [prefix, earlier, later] of repositories) {
    const remaining = new Map(later.map((entry) => [key(entry), entry]));
    for (const entry of earlier) {
      const next = remaining.get(key(entry));
      if (!next || JSON.stringify(entry) !== JSON.stringify(next)) {
        add(`${label(prefix + entry.path)} [${entry.status}]: ${entry.blob ?? "deleted"}/${entry.mode ?? "-"} -> ${next ? `${next.blob ?? "deleted"}/${next.mode ?? "-"}` : "record removed"}`);
      }
      remaining.delete(key(entry));
    }
    for (const entry of remaining.values()) add(`${label(prefix + entry.path)} [${entry.status}]: record added (${entry.blob ?? "deleted"}/${entry.mode ?? "-"})`);
  }
  return Buffer.from(lines.join("\n")).subarray(0, 4096).toString("utf8") + "\nComplete observations: results.json sourceIdBefore/sourceIdAfter";
}

export function runSourceCommand(binary, args, options) {
  return new Promise((resolveResult) => {
    const child = execFile(binary, args, options, (error, stdout, stderr) => {
      const interrupted = error && (error.killed || error.signal || typeof error.code !== "number");
      resolveResult({
        status: error ? (interrupted ? null : error.code) : 0,
        signal: error?.signal ?? null,
        error: interrupted ? error : undefined,
        stdout, stderr,
      });
    });
    child.stdin?.end();
  });
}

export async function captureCommitInputStatus(startDirectory = process.cwd(), sourceEnv = process.env, { run = runSourceCommand, pin } = {}) {
  // A native host's metadata pin is authoritative. Hostless setup is never a
  // fallback after a metadata refusal or an executable mismatch.
  const binary = pin ? verifySourceOwnerPin(pin) : sourceBinaryPath(sourceEnv);
  const root = resolve(startDirectory);
  const unavailable = (message, refusal) => Object.assign(new Error(
    `committed input unavailable (${binary}; ${pin ? "host-selected owner" : "set ENGRAM_SOURCE_BINARY to select an owner"}): ${message.slice(0, 4096)}`),
  { code: "ENGRAM_SOURCE_UNAVAILABLE", ...(refusal ? { refusal } : {}) });
  const warning = warnSlowEngramOperation("source observation");
  try {
    const result = await run(binary, ["source", "--root", root, "--json"], {
      cwd: root, env: sourceEnv, encoding: null, windowsHide: true, stdio: ["ignore", "pipe", "pipe"],
      // Owners choose their supported default (30 s before the sync, 60 s
      // after it). Fund the larger default plus process/envelope delivery.
      timeout: sourceProcessBudgetMs, maxBuffer: SOURCE_JSON_BYTES + 1,
    });
    if (pin && verifySourceOwnerPin(pin) !== binary) throw unavailable("source owner executable changed during capture");
    if (result.error || result.signal || ![0, 1].includes(result.status)) {
      throw unavailable(result.error?.message ?? `owner exited ${result.status ?? "without a status"}${result.signal ? ` (${result.signal})` : ""}`);
    }
    const output = result.stdout;
    if (!Buffer.isBuffer(output) || output.length > SOURCE_JSON_BYTES + 1 || result.stderr?.length) {
      throw unavailable("owner output exceeded its bound or carried process stderr");
    }
    let observation;
    try {
      const json = new TextDecoder("utf-8", { fatal: true }).decode(output);
      if (Buffer.byteLength(json.replace(/\n$/u, "")) > SOURCE_JSON_BYTES) throw new Error("JSON byte limit");
      observation = JSON.parse(json);
    } catch { throw unavailable("owner returned malformed, non-UTF-8 or oversized JSON"); }
    if (!fieldsExactly(observation, ["version", "status", "root", "source", "tree", "entries", "refusal", "source_id"])
        || observation.version !== 1 || !["clean", "not_clean", "unavailable"].includes(observation.status)
        || !Array.isArray(observation.entries)) throw unavailable("owner returned an unsupported JSON v1 envelope");
    const refused = observation.status === "unavailable";
    if (refused) {
      if (result.status !== 1 || observation.source !== null || observation.tree !== null || observation.entries.length !== 0
          || observation.source_id !== null
          || !(observation.root === null || (sourceText(observation.root) && isAbsolute(observation.root)))
          || !fieldsExactly(observation.refusal, ["code", "message"]) || !sourceCodes.has(observation.refusal.code)
          || !sourceText(observation.refusal.message) || Buffer.byteLength(observation.refusal.message) > 4096) {
        throw unavailable("owner returned an inconsistent unavailable envelope");
      }
      throw unavailable(`${observation.refusal.code}: ${observation.refusal.message}`, observation.refusal);
    }
    if (result.status !== 0 || observation.refusal !== null || !sourceText(observation.root) || !isAbsolute(observation.root)
        || typeof observation.source !== "string" || !/^git-commit:[a-f0-9]{40}$/u.test(observation.source)
        || typeof observation.tree !== "string" || !/^[a-f0-9]{40}$/u.test(observation.tree)
        || (observation.status === "clean") !== (observation.entries.length === 0)) {
      throw unavailable("owner returned an inconsistent completed envelope");
    }
    const sourceId = sourceValue(observation.source_id);
    if (sourceId === null || sourceId.commit !== observation.source.slice("git-commit:".length)
        || sourceId.tree !== observation.tree
        || (observation.status === "clean" && (sourceId.changes.length
          || sourceId.submodules.some((child) => child.changes.length)))) {
      throw unavailable("owner returned an inconsistent source_id");
    }
    for (const entry of observation.entries) {
      const renamed = /[RC]/u.test(entry?.status ?? "");
      if (!fieldsExactly(entry, renamed ? ["status", "path", "origin"] : ["status", "path"])
          || typeof entry.status !== "string" || !/^[ A-Z?!]{2}$/u.test(entry.status)
          || !sourceText(entry.path) || (renamed && !sourceText(entry.origin))) {
        throw unavailable("owner returned malformed changed-path metadata");
      }
    }
    try {
      if (realpathSync.native(observation.root) !== realpathSync.native(root)) throw new Error("root mismatch");
    } catch { throw unavailable("owner canonical root differs from the requested root or cannot be resolved"); }
    return { root: observation.root, commit: observation.source.slice("git-commit:".length), tree: observation.tree,
      status: observation.status, entries: observation.entries, sourceId };
  } finally { clearTimeout(warning); }
}
