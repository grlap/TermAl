// Detached Node survives launcher death. Its ordinary PowerShell child can
// initialize without DETACHED_PROCESS; Node's private job also owns that child.
// PowerShell holds the only strict command-job handle. No target runs outside it.
import { spawn } from "node:child_process";
import { appendFileSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmdirSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join, resolve, toNamespacedPath } from "node:path";
import { fileURLToPath } from "node:url";

const requestPath = process.argv[2];
const request = JSON.parse(readFileSync(requestPath, "utf8"));
const trace = (phase, fields = {}) => appendFileSync(`${requestPath}.bootstrap.jsonl`,
  `${JSON.stringify({ phase, pid: process.pid, ...fields })}\n`);
const powershell = join(process.env.SystemRoot ?? process.env.SYSTEMROOT ?? "C:/Windows",
  "System32", "WindowsPowerShell", "v1.0", "powershell.exe");
// Compiler-generated names need room beyond TEMP. Git's linked-worktree run
// directory can be deep even when this helper checkout has a short path.
const helperRoot = realpathSync.native(resolve(dirname(fileURLToPath(import.meta.url)), ".."));
const compilerTemp = join(helperRoot, ".tmp");
const compilerBase = join(compilerTemp, "ow");
const assertOwnedDirectory = (path) => {
  const info = lstatSync(path);
  if (info.isSymbolicLink() || !info.isDirectory()
    || realpathSync.native(path).toLowerCase() !== path.toLowerCase()) {
    throw new Error(`Windows owner compiler scratch refuses redirected directory: ${path}`);
  }
};
const ensureOwnedDirectory = (path) => {
  try { mkdirSync(path); }
  catch (error) { if (error.code !== "EEXIST") throw error; }
  assertOwnedDirectory(path);
};
if (join(compilerBase, "o-XXXXXX").length > 200) {
  throw new Error("Windows owner compiler scratch needs a shorter helper checkout path (maximum 200 characters including scratch name)");
}
// Check each owned component before descending; recursive mkdir would follow
// an existing .tmp/ow junction before we could validate the allocated leaf.
ensureOwnedDirectory(compilerTemp);
ensureOwnedDirectory(compilerBase);
const compilerScratch = mkdtempSync(join(compilerBase, "o-"));
assertOwnedDirectory(compilerScratch);
// Windows PowerShell's framework file APIs still impose MAX_PATH. Keep its
// request and bootstrap trace beside the existing short compiler scratch;
// Node archives the trace at the original, unshortened run path before exit.
const compilerRequest = join(compilerScratch, "request.json");
const bootstrapPath = join(compilerScratch, "bootstrap.jsonl");
writeFileSync(compilerRequest, JSON.stringify({
  ...request, log: toNamespacedPath(request.log), ledger: toNamespacedPath(request.ledger),
}), { flag: "wx" });
const args = ["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File",
  join(dirname(fileURLToPath(import.meta.url)), "test-process-owner-windows.ps1"),
  "-RequestPath", compilerRequest, "-CompilerScratch", compilerScratch,
  "-BootstrapPath", bootstrapPath];
trace("node-guardian-entered");
const child = spawn(powershell, args, {
  cwd: request.cwd, env: process.env, detached: false, windowsHide: true,
  stdio: ["pipe", "pipe", "pipe"],
});
trace("powershell-spawned", { bootstrapPid: child.pid });

// A dead launcher closes both its input and output. Drain output after EPIPE
// so the native owner can finish cleanup and its immutable ledger publication.
process.stdout.on("error", () => { child.stdout.unpipe(process.stdout); child.stdout.resume(); });
process.stderr.on("error", () => { child.stderr.unpipe(process.stderr); child.stderr.resume(); });
process.stdin.on("error", () => child.stdin.end());
child.stdin.on("error", () => {});
process.stdout.write(`TERMAL_OWNER ${JSON.stringify({ type: "bootstrap", nonce: request.nonce, pid: child.pid })}\n`);
process.stdin.pipe(child.stdin);
child.stdout.pipe(process.stdout, { end: false });
child.stderr.pipe(process.stderr, { end: false });
child.on("error", (error) => { process.stderr.write(`PowerShell bootstrap failed: ${error.message}\n`); });
child.on("close", (code, signal) => {
  let exitCode = code ?? 1;
  let bootstrapArchived = false;
  try {
    appendFileSync(`${requestPath}.bootstrap.jsonl`, readFileSync(bootstrapPath));
    bootstrapArchived = true;
  } catch (error) {
    trace("bootstrap-archive-failed", { bootstrapPath, error: error.message });
    process.stderr.write(`PowerShell bootstrap archive failed: ${error.message}\n`);
    exitCode = 1;
  }
  trace("powershell-close", { bootstrapPid: child.pid, code, signal });
  try {
    if (!bootstrapArchived) throw new Error("unarchived bootstrap evidence must be retained");
    assertOwnedDirectory(compilerTemp);
    assertOwnedDirectory(compilerBase);
    assertOwnedDirectory(compilerScratch);
    // Only unlink direct entries in this guardian's freshly allocated directory.
    // Never recurse into a directory or follow a link to someone else's files.
    for (const entry of readdirSync(compilerScratch, { withFileTypes: true })) {
      if (entry.isDirectory()) throw new Error("unexpected directory in compiler scratch");
      unlinkSync(join(compilerScratch, entry.name));
    }
    rmdirSync(compilerScratch);
    trace("compiler-scratch-removed", { compilerScratch });
  } catch (error) {
    trace("compiler-scratch-retained", { compilerScratch, error: error.message });
  }
  process.stdin.destroy();
  process.exitCode = exitCode;
});
