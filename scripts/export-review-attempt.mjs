import fs from "node:fs/promises";
import path from "node:path";
import crypto from "node:crypto";
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const digest = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const identifier = (value) => typeof value === "string" && /^[A-Za-z0-9_-]{1,200}$/.test(value);
const maxOutputBytes = 16 * 1024 * 1024;

async function checkDirectory(root, directory) {
  const relative = path.relative(root, directory);
  if (relative.startsWith("..") || path.isAbsolute(relative)) throw new Error("export path must stay inside the repository");
  let current = root;
  for (const part of ["", ...relative.split(path.sep).filter(Boolean)]) {
    if (part) current = path.join(current, part);
    try {
      const info = await fs.lstat(current);
      if (info.isSymbolicLink() || !info.isDirectory()) throw new Error(`export ancestor is not a plain directory: ${current}`);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
      await fs.mkdir(current);
    }
  }
}

const canonical = (value) => JSON.stringify(value, (_key, item) => item && typeof item === "object"
  && !Array.isArray(item) ? Object.fromEntries(Object.entries(item).sort(([a], [b]) => a.localeCompare(b))) : item);
function stableResponse(bytes) {
  const { revision: _revision, serverInstanceId: _instance,
    reviewAttemptState: _observation, ...retained } = JSON.parse(bytes.toString("utf8"));
  return canonical(retained);
}

function retainedManifestIdentity(manifest) {
  // Requester identity is observation provenance; retain the first export.
  const { requesterSessionId: _requester, parentSessionId: _legacyRequester, ...retained } = manifest;
  return retained;
}

async function identicalExport(directory, files, manifest) {
  try {
    const names = await fs.readdir(directory);
    if (names.length !== files.size || names.some((name) => !files.has(name))) return false;
    const saved = new Map();
    for (const [name, bytes] of files) {
      const location = path.join(directory, name);
      const info = await fs.lstat(location);
      if (!info.isFile() || info.isSymbolicLink()) return false;
      const original = await fs.readFile(location);
      saved.set(name, original);
      if (name === "packet.json" || name.startsWith("output-page-")) {
        if (stableResponse(original) !== stableResponse(bytes)) return false;
      } else if (name !== "manifest.json" && !original.equals(bytes)) return false;
    }
    // Preserve and verify the first observation's raw bytes and hashes. Only
    // volatile response-envelope fields may differ on a later observation.
    const originalManifest = JSON.parse(saved.get("manifest.json").toString("utf8"));
    const originalDescriptor = (descriptor) => ({ ...descriptor,
      bytes: saved.get(descriptor.path).length, sha256: digest(saved.get(descriptor.path)) });
    const expected = { ...manifest, packet: originalDescriptor(manifest.packet),
      pages: manifest.pages.map(originalDescriptor) };
    return canonical(retainedManifestIdentity(originalManifest)) === canonical(retainedManifestIdentity(expected)) ? originalManifest : false;
  } catch (error) {
    if (error.code === "ENOENT") return false;
    throw error;
  }
}

/** Export one pinned attempt. Existing differing artifacts are never replaced. */
export async function exportReviewAttempt({ baseUrl, parentSessionId, delegationId, submissionAttempt,
  resultId: expectedResultId, outputRoot, repositoryRoot, fetcher = fetch }) {
  if (!identifier(parentSessionId) || !identifier(delegationId)
      || !Number.isSafeInteger(submissionAttempt) || submissionAttempt < 1) throw new Error("explicit parent, delegation and positive submission attempt are required");
  if (expectedResultId !== undefined && !identifier(expectedResultId)) throw new Error("invalid explicit result identity");
  const repository = path.resolve(repositoryRoot ?? execFileSync("git", ["rev-parse", "--show-toplevel"], { encoding: "utf8" }).trim());
  const destinationRoot = path.resolve(outputRoot ?? path.join(repository, ".evidence", "review-attempts"));
  const url = new URL(baseUrl);
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password) throw new Error("invalid host URL");
  const endpoint = `/api/sessions/${parentSessionId}/delegations/${delegationId}/result`;
  async function readJson(suffix) {
    const response = await fetcher(new URL(endpoint + suffix, url));
    if (!response.ok) throw new Error(`review read failed: HTTP ${response.status}`);
    const bytes = Buffer.from(await response.arrayBuffer());
    if (bytes.length > maxOutputBytes) throw new Error("review response exceeds export bound");
    return { bytes, value: JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)) };
  }
  const packet = await readJson(`?submissionAttempt=${submissionAttempt}${expectedResultId ? `&resultId=${encodeURIComponent(expectedResultId)}` : ""}`);
  const resultId = packet.value.resultId;
  if (packet.value.submissionAttempt !== submissionAttempt || !identifier(resultId)
      || (expectedResultId !== undefined && resultId !== expectedResultId)
      || packet.value.result?.delegationId !== delegationId) throw new Error("packet does not identify the requested attempt");
  const files = new Map([["packet.json", packet.bytes]]);
  const pages = [];
  const chunks = [];
  let offset = 0;
  let totalBytes;
  const outputAvailable = packet.value.reviewAttemptProvenance?.rawOutputAvailable !== false;
  for (let pageNumber = 0; outputAvailable; pageNumber++) {
    if (pageNumber >= 4096) throw new Error("too many review output pages");
    const page = await readJson(`/output?submissionAttempt=${submissionAttempt}&resultId=${encodeURIComponent(resultId)}&offsetBytes=${offset}&limitBytes=8192`);
    const p = page.value;
    const bytes = Buffer.from(p.output ?? "", "utf8");
    if (p.delegationId !== delegationId || p.childSessionId !== packet.value.result.childSessionId
        || p.submissionAttempt !== submissionAttempt || p.resultId !== resultId || p.summaryFallback !== false
        || p.offsetBytes !== offset || typeof p.output !== "string" || bytes.toString("utf8") !== p.output || !Number.isSafeInteger(p.totalBytes)
        || p.totalBytes < 0 || p.totalBytes > maxOutputBytes || typeof p.complete !== "boolean"
        || (totalBytes !== undefined && totalBytes !== p.totalBytes)) throw new Error("unavailable, mixed or invalid review output page");
    totalBytes = p.totalBytes;
    const next = offset + bytes.length;
    if (next > totalBytes || (p.complete ? next !== totalBytes || p.nextOffsetBytes != null
      : bytes.length === 0 || p.nextOffsetBytes !== next)) throw new Error("review output page has a gap or invalid terminal boundary");
    const name = `output-page-${String(pageNumber).padStart(5, "0")}.json`;
    files.set(name, page.bytes);
    pages.push({ path: name, offsetBytes: offset, bytes: page.bytes.length, sha256: digest(page.bytes), complete: p.complete });
    chunks.push(bytes);
    offset = next;
    if (p.complete) break;
  }
  const output = Buffer.concat(chunks);
  if (outputAvailable) files.set("full-output.txt", output);
  const manifest = { schemaVersion: 1, requesterSessionId: parentSessionId, delegationId, submissionAttempt, resultId,
    packet: { path: "packet.json", bytes: packet.bytes.length, sha256: digest(packet.bytes) }, pages,
    output: outputAvailable
      ? { available: true, path: "full-output.txt", bytes: output.length, sha256: digest(output), complete: true }
      : { available: false, complete: false } };
  files.set("manifest.json", Buffer.from(JSON.stringify(manifest, null, 2) + "\n"));
  await checkDirectory(repository, destinationRoot);
  const parent = path.join(destinationRoot, delegationId);
  await checkDirectory(repository, parent);
  const attemptDirectory = path.join(parent, `attempt-${submissionAttempt}`);
  await checkDirectory(repository, attemptDirectory);
  const destination = path.join(attemptDirectory, resultId);
  try {
    const info = await fs.lstat(destination);
    if (!info.isDirectory() || info.isSymbolicLink()) throw new Error("export destination is not a plain directory");
    const existing = await identicalExport(destination, files, manifest);
    if (existing) return { directory: destination, unchanged: true, manifest: existing };
    throw new Error("immutable attempt export already exists with different bytes");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const staging = path.join(attemptDirectory, `.export-${crypto.randomUUID()}`);
  await fs.mkdir(staging);
  const written = [];
  let publishedManifest = manifest;
  let unchanged = false;
  try {
    for (const [name, bytes] of files) {
      await fs.writeFile(path.join(staging, name), bytes, { flag: "wx" });
      written.push(name);
    }
    try { await fs.rename(staging, destination); }
    catch (error) {
      const existing = await identicalExport(destination, files, manifest);
      if (!existing) throw error;
      publishedManifest = existing;
      unchanged = true;
      for (const name of written) await fs.unlink(path.join(staging, name));
      await fs.rmdir(staging);
    }
  } catch (error) {
    // Only the exact files created by this invocation are eligible for cleanup.
    for (const name of written) await fs.unlink(path.join(staging, name)).catch(() => {});
    await fs.rmdir(staging).catch(() => {});
    throw error;
  }
  return { directory: destination, unchanged, manifest: publishedManifest };
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const args = process.argv.slice(2);
  if (![10, 12].includes(args.length) || args[0] !== "--base-url" || args[2] !== "--parent" || args[4] !== "--delegation"
      || args[6] !== "--attempt" || args[8] !== "--output-root" || (args.length === 12 && args[10] !== "--result-id")) {
    throw new Error("usage: node scripts/export-review-attempt.mjs --base-url URL --parent ID --delegation ID --attempt N --output-root PATH [--result-id ID]");
  }
  const result = await exportReviewAttempt({ baseUrl: args[1], parentSessionId: args[3], delegationId: args[5],
    submissionAttempt: Number(args[7]), outputRoot: args[9], resultId: args[11] });
  console.log(JSON.stringify({ directory: result.directory, unchanged: result.unchanged, resultId: result.manifest.resultId }));
}
