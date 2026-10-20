// Owns one focused command's lifetime; it never enumerates or kills by name.
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { openSync, closeSync, readFileSync, writeFileSync, appendFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const directory = dirname(fileURLToPath(import.meta.url));
export const PROCESS_CLEANUP_BUDGET_MS = 5000;
const interruptReason = (signal) => `interrupt: ${String(signal.reason ?? "requested").slice(0, 512)}`;

export async function runOwnedCommand(command, args, { cwd, env, log, signal, onStart, onOwner }, {
  spawnOwner = spawn, platform = process.platform,
} = {}) {
  const fd = openSync(log, "wx");
  closeSync(fd);
  const nonce = randomUUID();
  const requestPath = `${log}.owner-request.json`;
  const ledger = `${log}.owner-result.json`;
  const bootstrap = `${requestPath}.bootstrap.jsonl`;
  writeFileSync(bootstrap, `${JSON.stringify({ phase: "launcher-owner-request", pid: process.pid })}\n`, { flag: "wx" });
  writeFileSync(requestPath, JSON.stringify({
    command, args, cwd, log, ledger, nonce, cleanupBudgetMs: PROCESS_CLEANUP_BUDGET_MS,
  }), { flag: "wx" });
  if (signal?.aborted) return {
    code: 1, signal: null, interrupted: true,
    cleanup: { state: "reaped", trigger: interruptReason(signal), pids: [] },
  };
  return new Promise((resolve) => {
    const windows = platform === "win32";
    const program = process.execPath;
    const ownerArgs = windows
      ? [join(directory, "test-process-owner-windows.mjs"), requestPath]
      : [join(directory, "test-process-owner-posix.mjs"), requestPath];
    appendFileSync(bootstrap, `${JSON.stringify({ phase: "owner-spawn", program, args: ownerArgs, cwd, detached: true })}\n`);
    const owner = spawnOwner(program, ownerArgs, {
      // In particular, skip libuv's Windows parent-private job: this owner
      // must survive launcher death long enough to write its cleanup ledger.
      cwd, env, detached: true, windowsHide: true,
      stdio: windows ? ["pipe", "pipe", "pipe"] : ["ignore", "ignore", "pipe", "ipc"],
    });
    let result, protocolError, stderr = "", buffer = "", interrupted, bootstrapPid;
    let environmentSent = !windows, pendingStop;
    const requestStop = (reason) => {
      pendingStop ??= reason;
      try {
        if (windows) {
          if (!environmentSent) return;
          if (!owner.stdin.destroyed && !owner.stdin.writableEnded) owner.stdin.end(`${reason}\n`);
        }
        else if (owner.connected) owner.send({ type: "interrupt", reason }, () => {});
      } catch (error) { protocolError ??= `owner control failed: ${error.message}`; }
    };
    if (windows) {
      // Keep credentials/environment off disk and out of control output.
      // PowerShell startup may alter its own environment; the command gets
      // exactly the caller's environment instead of that altered copy.
      owner.stdout.setEncoding("utf8");
    }
    owner.stderr.setEncoding("utf8");
    const abort = () => {
      interrupted ??= interruptReason(signal);
      requestStop(interrupted);
    };
    owner.stdin?.on("error", () => {}); // A closed owner is judged from its ledger/close event.
    const receive = (message) => {
      if (message?.nonce !== nonce) { protocolError ??= "process owner nonce mismatch"; return; }
      if (message.type === "started") {
        // POSIX IPC can drop a signal sent before the owner's ESM listener
        // exists. This acknowledgement proves that listener is now installed.
        if (pendingStop) requestStop(pendingStop);
        try { onStart?.(message.pid); }
        catch (error) {
          protocolError ??= error.message;
          requestStop("interrupt: observation failed");
        }
      } else if (message.type === "bootstrap") {
        bootstrapPid = message.pid;
        try { onOwner?.({ pid: owner.pid, ledger, nonce, bootstrapPid }); }
        catch (error) { protocolError ??= error.message; requestStop("interrupt: bootstrap observation failed"); }
      } else if (message.type === "finished") result = message.result;
    };
    if (windows) owner.stdout.on("data", (chunk) => {
      buffer += chunk;
      if (buffer.length > 262144) { protocolError ??= "process owner response exceeded limit"; requestStop("interrupt: invalid owner response"); return; }
      let newline;
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline).trim(); buffer = buffer.slice(newline + 1);
        // Bootstrap warnings are diagnostic output, never control messages.
        if (!line.startsWith("TERMAL_OWNER ")) continue;
        try { receive(JSON.parse(line.slice("TERMAL_OWNER ".length))); } catch (error) { protocolError ??= error.message; requestStop("interrupt: invalid owner response"); }
      }
    });
    else owner.on("message", receive);
    owner.stderr.on("data", (chunk) => { stderr = (stderr + chunk).slice(-2400); });
    owner.once("error", (error) => { protocolError ??= error.message; });
    owner.once("close", (code, closeSignal) => {
      signal?.removeEventListener("abort", abort);
      const ownerDiagnostics = { code, signal: closeSignal, stderr };
      try { appendFileSync(bootstrap, `${JSON.stringify({ phase: "owner-close", pid: owner.pid, code, signal: closeSignal })}\n`); }
      catch (error) { protocolError ??= `owner trace failed: ${error.message}`; }
      if (code !== 0) protocolError ??= `process owner failed: exit=${code ?? "unknown"} signal=${closeSignal ?? "none"}${stderr ? `; ${stderr}` : ""}`;
      try {
        const durable = JSON.parse(readFileSync(ledger, "utf8"));
        if (durable.nonce !== nonce || JSON.stringify(durable) !== JSON.stringify(result)) throw new Error("missing or inconsistent process owner result");
      } catch (error) { protocolError ??= error.message; }
      if (!result) result = { code: 1, signal: closeSignal, cleanup: { state: "unresolved", trigger: "owner-failure", pids: [] } };
      if (signal?.aborted) {
        result.interrupted = true; result.code = 1;
        result.cleanup = { ...result.cleanup, ownerTrigger: result.cleanup.trigger, trigger: interruptReason(signal) };
      }
      if (protocolError) { result.code = 1; result.error = protocolError; }
      resolve({ ...result, ownerDiagnostics, ownerPid: owner.pid, bootstrapPid, ownerLedger: ledger });
    });
    try { if (Number.isSafeInteger(owner.pid)) onOwner?.({ pid: owner.pid, ledger, nonce }); }
    catch (error) { protocolError ??= error.message; requestStop("interrupt: owner observation failed"); }
    if (windows) {
      // Admit environment only after the owner identity was recorded. A
      // cancellation in that observation cannot race native command startup:
      // the helper is still blocked waiting for this exact private pipe.
      environmentSent = true;
      if (pendingStop || signal?.aborted) owner.stdin.end();
      else owner.stdin.write(`${JSON.stringify(env)}\n`);
    }
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) abort();
  });
}
