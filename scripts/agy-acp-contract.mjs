// One ACP contract probe, with no login, credential/configuration access, or retry.
// Used only by the opt-in test below; not part of the default full gate.
import { spawn } from "node:child_process";
import { StringDecoder } from "node:string_decoder";

export async function probeAgy({ binary, cwd, launch = spawn, deadlineMs = 90_000 }) {
  const child = launch(binary, ["acp"], {
    cwd, stdio: ["pipe", "pipe", "pipe"], windowsHide: true,
  });
  const pending = new Map();
  let nextId = 0, buffer = "", bytes = 0, sessionId, permissionSeen = false;
  let completed = false, failure, closed = false;
  const report = { protocolVersion: null, authMethods: [], permissionResponse: null,
    loadSession: null, resume: null, promptCount: 0, cancelSent: false };
  const decoder = new StringDecoder("utf8");
  let rejectFailure;
  const failed = new Promise((_, reject) => { rejectFailure = reject; });
  // Failures can arrive before the first awaited operation.
  failed.catch(() => {});
  const fail = error => {
    if (failure) return;
    failure = error;
    rejectFailure(error);
    for (const { reject } of pending.values()) reject(error);
    pending.clear();
  };
  const send = message => {
    if (failure) throw failure;
    child.stdin.write(JSON.stringify(message) + "\n");
  };
  const request = (method, params) => {
    const id = ++nextId;
    const response = new Promise((resolve, reject) => pending.set(id, { resolve, reject, method }));
    send({ jsonrpc: "2.0", id, method, params });
    return Promise.race([response, failed]);
  };
  const receive = message => {
    if (message.method === "session/request_permission" && message.id != null) {
      if (!sessionId || message.params?.sessionId !== sessionId) {
        throw new Error("permission request belongs to another session");
      }
      if (permissionSeen) throw new Error("smoke requested more than one permission");
      permissionSeen = true;
      const denied = message.params?.options?.find(option => option.kind === "reject_once");
      send({ jsonrpc: "2.0", id: message.id, result: { outcome: denied
        ? { outcome: "selected", optionId: denied.optionId } : { outcome: "cancelled" } } });
      report.permissionResponse = denied ? "reject_once" : "cancelled";
      // Exercise cancellation while this single prompt is active. Never execute
      // the requested command, even if the provider changes its tool choice.
      send({ jsonrpc: "2.0", method: "session/cancel", params: { sessionId } });
      report.cancelSent = true;
    } else if (message.method && message.id != null) {
      throw new Error("unexpected ACP client request: " + String(message.method).slice(0, 80));
    } else if (message.id != null) {
      const entry = pending.get(message.id);
      if (!entry) throw new Error("unexpected ACP response id");
      pending.delete(message.id);
      if (message.error) {
        const auth = /auth|log.?in|sign.?in/i.test(String(message.error.message));
        entry.reject(new Error(auth ? "blocked-on-auth at " + entry.method
          : "ACP failure at " + entry.method + " (code " + message.error.code + ")"));
      } else {
        entry.resolve(message.result);
      }
    }
  };
  const exited = new Promise(resolve => child.once("close", () => {
    closed = true;
    if (!completed) fail(new Error("agy exited before the ACP contract completed"));
    resolve();
  }));
  child.once("error", fail);
  child.stdin.on("error", fail);
  // Discard stderr without printing settings, tokens, model text or diagnostics.
  child.stderr.resume();
  child.stdout.on("data", chunk => {
    try {
      bytes += chunk.length;
      if (bytes > 2 * 1024 * 1024) throw new Error("ACP smoke output exceeded its byte bound");
      buffer += decoder.write(chunk);
      let newline;
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline).trim();
        buffer = buffer.slice(newline + 1);
        if (line) receive(JSON.parse(line));
      }
      if (buffer.length > 1024 * 1024) throw new Error("ACP smoke frame exceeded its byte bound");
    } catch (error) { fail(error); }
  });
  const deadline = setTimeout(() => fail(new Error("ACP smoke deadline exceeded")), deadlineMs);
  try {
    const initialized = await request("initialize", {
      protocolVersion: 1,
      clientInfo: { name: "termal", version: "contract-smoke" },
      clientCapabilities: {},
    });
    report.protocolVersion = initialized?.protocolVersion;
    report.authMethods = (initialized?.authMethods ?? []).map(method => ({
      id: String(method.id).slice(0, 128), name: String(method.name ?? "").slice(0, 128),
    }));
    report.agentInfo = { name: String(initialized?.agentInfo?.name ?? "").slice(0, 128),
      version: String(initialized?.agentInfo?.version ?? "").slice(0, 128) };
    const load = initialized?.agentCapabilities?.loadSession;
    const resume = initialized?.agentCapabilities?.sessionCapabilities?.resume;
    report.loadSession = typeof load === "boolean" ? load : null;
    report.resume = resume !== null && typeof resume === "object" && !Array.isArray(resume);
    if (report.protocolVersion !== 1) throw new Error("agy did not negotiate ACP protocol 1");
    const session = await request("session/new", { cwd, mcpServers: [] });
    sessionId = session?.sessionId;
    if (typeof sessionId !== "string" || !sessionId) throw new Error("session/new returned no id");
    report.promptCount++;
    const result = await request("session/prompt", {
      sessionId, prompt: [{ type: "text", text:
        "Protocol smoke test only. Request permission to run exactly the harmless shell command echo TERMAL_ACP_SMOKE. Do not edit files, inspect credentials or settings, use network tools, or do any other work. If denied or cancelled, stop immediately." }],
    });
    report.stopReason = result?.stopReason;
    if (!permissionSeen) throw new Error("permission response not observed; do not retry for quota");
    if (!report.cancelSent) throw new Error("cancel was not sent");
    if (report.stopReason !== "cancelled") throw new Error("prompt cancellation was not observed");
    completed = true;
    return report;
  } catch (error) {
    error.report = report;
    throw error;
  } finally {
    clearTimeout(deadline);
    completed = true;
    if (!closed) child.kill();
    let cleanupTimer;
    try {
      await Promise.race([exited, new Promise((_, reject) => {
        cleanupTimer = setTimeout(() => reject(new Error("agy child did not exit after cleanup")), 5_000);
      })]);
    } finally { clearTimeout(cleanupTimer); }
  }
}
