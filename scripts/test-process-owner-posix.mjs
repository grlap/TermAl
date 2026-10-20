// Separate from the launcher: IPC closure survives a SIGKILL of its parent.
import { spawn } from "node:child_process";
import { openSync, closeSync, fsyncSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

// Inject only OS observations/transport for deterministic failure-path tests.
// Production always uses the real group signal, child and private parent IPC.
export function startPosixOwner(request, {
  parent = process, spawnCommand = spawn,
  killGroup = (pid) => process.kill(-pid, "SIGKILL"),
  groupAlive = (pid) => {
    try { process.kill(-pid, 0); return true; }
    catch (error) { if (error.code === "ESRCH") return false; throw error; }
  },
  now = () => performance.now(), sleep = () => new Promise((done) => setTimeout(done, 10)),
  openLog = () => openSync(request.log, "a"), closeLog = closeSync,
  persist = (result) => {
    const partial = `${request.ledger}.partial`;
    const fd = openSync(partial, "wx");
    try { writeFileSync(fd, JSON.stringify(result)); fsyncSync(fd); }
    finally { closeSync(fd); }
    renameSync(partial, request.ledger);
  },
} = {}) {
  const fd = openLog();
  let child, reason, ending;
  let rootOutcome = { code: 1, signal: null };
  const send = (message) => new Promise((done) => {
    if (!parent.connected) { done(new Error("launcher-exit")); return; }
    try { parent.send(message, (error) => done(error)); }
    catch (error) { done(error); }
  });
  const interrupt = (trigger) => { reason ??= trigger; return finish(reason); };
  async function finish(trigger) {
    if (ending) return ending;
    ending = (async () => {
      let cleanup = "reaped", error;
      try {
        if (child?.pid) {
          try { killGroup(child.pid); }
          catch (failure) { if (failure.code !== "ESRCH") throw failure; }
          const started = now();
          while (groupAlive(child.pid) && now() - started < request.cleanupBudgetMs) await sleep();
          if (groupAlive(child.pid)) { cleanup = "unresolved"; error = "owned process group did not exit within cleanup budget"; }
        }
      } catch (failure) { cleanup = "unresolved"; error = failure.message; }
      const result = {
        ...rootOutcome, nonce: request.nonce, pid: child?.pid ?? null,
        interrupted: Boolean(reason),
        cleanup: { state: cleanup, trigger, pids: child?.pid ? [child.pid] : [],
          pidScope: "command root only", ownership: "posix-process-group" },
        ...(error ? { error } : {}),
      };
      if (reason || error) result.code = 1;
      try {
        persist(result);
        await send({ type: "finished", nonce: request.nonce, result });
      } finally {
        // A live/uninterruptible root must not keep this observer alive after
        // the budget. The durable result stays unresolved, never reaped/PASS.
        child?.unref();
        closeLog(fd);
        if (parent.connected) parent.disconnect();
      }
      return result;
    })();
    return ending;
  }
  parent.on("message", (message) => {
    if (message?.type === "interrupt") void interrupt(message.reason ?? "interrupt");
  });
  parent.on("disconnect", () => { void interrupt("launcher-exit"); });
  // IPC can fail before its disconnect event. Both paths own the same cleanup.
  parent.on("error", () => { void interrupt("launcher-exit"); });
  if (!parent.connected) { void interrupt("launcher-exit"); return { finish }; }
  child = spawnCommand(request.command, request.args, {
    cwd: request.cwd, env: parent.env, detached: true, stdio: ["ignore", fd, fd],
  });
  child.once("spawn", () => {
    void send({ type: "started", nonce: request.nonce, pid: child.pid, ownerPid: parent.pid })
      .then((error) => { if (error) return interrupt("launcher-exit"); });
  });
  child.once("error", (error) => { rootOutcome.error = error.message; void finish("spawn-failure"); });
  child.once("exit", (code, signal) => { rootOutcome = { code, signal }; void finish(reason ?? "command-exit"); });
  return { finish };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  startPosixOwner(JSON.parse(readFileSync(process.argv[2], "utf8")));
}
