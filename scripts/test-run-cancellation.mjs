// Carries run-local stop requests. The worker keeps the launch key in memory,
// authenticates bounded regular-file records and rechecks current stall evidence.
import { createHash, createHmac, randomUUID, timingSafeEqual } from "node:crypto";
import { closeSync, constants, fstatSync, linkSync, lstatSync, openSync, readSync,
  statSync, unlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const controlFile = "cancel-request.json";
const pollIntervalMs = 250;
const maxRequestBytes = 2048;

export const cancelKeyEnvironment = "TERMAL_LAUNCHER_CANCEL_KEY";

export function scrubCancellationEnvironment(env) {
  for (const key of Object.keys(env)) {
    if (key.toUpperCase() === cancelKeyEnvironment) delete env[key];
  }
  return env;
}

function keyDigest(key) {
  return createHash("sha256").update(key).digest("hex");
}

function requestMac(key, runId, stall) {
  return createHmac("sha256", key).update(JSON.stringify([runId, stall ?? null])).digest("hex");
}

function digestMatches(expected, supplied) {
  return typeof supplied === "string" && /^[a-f0-9]{64}$/u.test(supplied) &&
    timingSafeEqual(Buffer.from(expected, "hex"), Buffer.from(supplied, "hex"));
}

function readControl(file) {
  // Reject links and special entries before opening. On POSIX the flags
  // also fence a replacement between lstat and open, including a FIFO
  // without a writer. Size and bytes are checked through the same handle.
  if (!lstatSync(file, { throwIfNoEntry: false })?.isFile()) return undefined;
  const fd = openSync(file, constants.O_RDONLY | (constants.O_NONBLOCK ?? 0) | (constants.O_NOFOLLOW ?? 0));
  try {
    const status = fstatSync(fd);
    if (!status.isFile()) return undefined;
    if (status.size > maxRequestBytes) return null;
    const bytes = Buffer.alloc(maxRequestBytes + 1);
    let length = 0;
    while (length < bytes.length) {
      const count = readSync(fd, bytes, length, bytes.length - length, null);
      if (!count) break;
      length += count;
    }
    if (length > maxRequestBytes) return null;
    return JSON.parse(bytes.subarray(0, length).toString("utf8"));
  } finally {
    closeSync(fd);
  }
}

export function requestRunCancellation(runDir, request, result, key, stalledOnly) {
  if (result.runId !== request.runId || result.state !== "running" || result.ended ||
      typeof result.cancellationKeyDigest !== "string") {
    throw new Error("this run is no longer active or does not support launcher cancellation");
  }
  if (!key || !digestMatches(keyDigest(key), result.cancellationKeyDigest)) {
    throw new Error(`cancellation requires the launch key in ${cancelKeyEnvironment}`);
  }
  const record = {};
  if (stalledOnly) {
    const entry = [...(result.preflight ?? []), ...(result.toolchainProbes ?? []), ...result.stages]
      .find((entry) => entry.state === "running" && entry.stall?.state === "stalled");
    if (!entry) throw new Error("no current stalled command");
    record.stall = { stage: entry.name, episode: entry.stall.episode, pid: entry.stall.pid };
  }
  record.mac = requestMac(key, request.runId, record.stall);
  const payload = JSON.stringify(record);
  if (Buffer.byteLength(payload) > maxRequestBytes) throw new Error("cancellation request exceeds its size limit");
  const destination = join(runDir, controlFile);
  const temporary = join(runDir, `${controlFile}.${randomUUID()}.tmp`);
  writeFileSync(temporary, payload, { flag: "wx", mode: 0o600 });
  try {
    linkSync(temporary, destination);
  } catch (error) {
    if (error.code !== "EEXIST") throw error;
    if (lstatSync(destination, { throwIfNoEntry: false })?.isFile() === false) {
      throw new Error(`non-regular entry obstructs cancellation: inspect ${destination} without following it and remove the obstruction before retrying`);
    }
    throw new Error("this run already has a pending cancellation request");
  } finally {
    unlinkSync(temporary);
  }
}

export function watchRunCancellation(runDir, request, result, controller, key) {
  result.cancellationKeyDigest = keyDigest(key);
  const file = join(runDir, controlFile);
  let stalled;
  let commandStop;
  let requested = false;
  let closed = false;
  const stopRun = (reason) => {
    requested = true;
    controller.abort(reason);
  };
  const check = () => {
    if (closed || controller.signal.aborted) return;
    let control;
    const clearRequest = () => {
      try { unlinkSync(file); } catch { /* Another tick can clear a stale request. */ }
    };
    try {
      control = readControl(file);
    } catch (error) {
      if (error instanceof SyntaxError) clearRequest();
      return;
    }
    if (control === undefined) return;
    if (!digestMatches(requestMac(key, request.runId, control?.stall), control?.mac)) {
      clearRequest();
      return;
    }
    if (control.stall) {
      let current = false;
      try {
        current = commandStop && stalled && stalled.stage === control.stall.stage &&
          stalled.episode === control.stall.episode && stalled.pid === control.stall.pid &&
          statSync(stalled.log).size === stalled.bytes;
      } catch { /* Inability to confirm current inactivity refuses the request. */ }
      clearRequest();
      if (current) commandStop.abort("operator cancelled a stalled command");
      return;
    }
    stopRun("operator cancelled the run");
  };
  const timer = setInterval(check, pollIntervalMs);
  timer.unref?.();
  return {
    check,
    startCommand() {
      commandStop = new AbortController();
      return AbortSignal.any([controller.signal, commandStop.signal]);
    },
    settleCommand(outcome) {
      if (commandStop?.signal.aborted && outcome.interrupted) {
        stopRun(commandStop.signal.reason);
      }
    },
    observe(observation) {
      stalled = observation.state === "stalled" ? {
        ...observation, bytes: observation.logBytes,
      } : undefined;
    },
    clearCommand() { stalled = undefined; commandStop = undefined; },
    wasRequested() { return requested; },
    close() {
      closed = true;
      clearInterval(timer);
      delete result.cancellationKeyDigest;
    },
  };
}
