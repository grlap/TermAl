// Split from the launch boundary in scripts/test-launcher.mjs. Owns the
// bounded host registration handshake, not execution, credit or restart policy.
import { createHash } from "node:crypto";
import { realpathSync } from "node:fs";

const contextVariables = ["TERMAL_BASE_URL", "TERMAL_SESSION_ID", "TERMAL_CLI"];
const registrationBudgetMs = 8_000;
const responseByteLimit = 65_536;
const contract = "termal-durable-test-run-registration";
const version = 1;
const immutableFields = ["runId", "root", "stages", "notifyTo", "full", "scope", "detached",
  "creatorPid", "owner", "started", "requiredBinaryEnv", "prerequisiteFiles", "needsCargo", "liveEngram"];

function ordered(value) {
  if (Array.isArray(value)) return value.map(ordered);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.keys(value).sort().map((key) => [key, ordered(value[key])]));
  }
  return value;
}

function requestIdentity(request) {
  const immutable = Object.fromEntries(immutableFields.map((name) => [name, request[name] ?? null]));
  return createHash("sha256").update(JSON.stringify(ordered(immutable))).digest("hex");
}

function pathIdentity(path) {
  if (typeof path !== "string" || !path) {
    throw new Error("registration error: missing or malformed registration path");
  }
  let canonical;
  try { canonical = realpathSync.native(path); }
  catch { throw new Error("registration error: registration path is unavailable or inaccessible"); }
  const key = canonical.replace(/^\\\\\?\\/u, "").replaceAll("\\", "/");
  return process.platform === "win32" ? key.toLowerCase() : key;
}

export function hasTermalContext(env) {
  return contextVariables.some((name) => env[name] !== undefined);
}

function hostConfiguration(env) {
  const session = env.TERMAL_SESSION_ID;
  if (typeof session !== "string" || !/^[A-Za-z0-9_-]{1,128}$/u.test(session)) {
    throw new Error("registration error: missing or malformed TERMAL_SESSION_ID");
  }
  let base;
  try { base = new URL(env.TERMAL_BASE_URL); } catch {
    throw new Error("registration error: missing or malformed TERMAL_BASE_URL");
  }
  if (!["http:", "https:"].includes(base.protocol) || base.username || base.password || base.search || base.hash) {
    throw new Error("registration error: unsupported TermAl host URL");
  }
  return { session, base, url: new URL(`/api/sessions/${encodeURIComponent(session)}/test-runs`, base) };
}

export function validateRegistrationContext(env) {
  if (hasTermalContext(env)) hostConfiguration(env);
}

async function readResponse(response) {
  if (!response.body) throw new Error("registration error: host response has no body");
  let length = 0;
  const chunks = [];
  for await (const chunk of response.body) {
    length += chunk.length;
    if (length > responseByteLimit) throw new Error("registration error: oversized host response");
    chunks.push(chunk);
  }
  try { return JSON.parse(Buffer.concat(chunks).toString("utf8")); }
  catch { throw new Error("registration error: malformed host response"); }
}

function validateAcknowledgement(acknowledgement, runId) {
  if (!acknowledgement || acknowledgement.contract !== contract || acknowledgement.version !== version
      || acknowledgement.runId !== runId || acknowledgement.registered !== true) {
    throw new Error("registration error: host did not acknowledge this exact run with the supported durable contract");
  }
}

async function queryRegisteredRun(request, runDirectory, configuration, deadline) {
  const remaining = deadline - Date.now();
  if (remaining <= 0) throw new Error("registration error: host query budget exhausted; no tests started");
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), remaining);
  try {
    const response = await fetch(new URL(`/api/test-runs/${encodeURIComponent(request.runId)}`, configuration.base), {
      signal: controller.signal, redirect: "error",
    });
    if (!response.ok) {
      await response.body?.cancel();
      throw new Error("registration error: acknowledged run is unavailable in the host query");
    }
    const { run } = await readResponse(response);
    if (!run || run.runId !== request.runId || run.ownerSessionId !== configuration.session
        || run.initiatingSessionId !== configuration.session || run.registrationState !== "registered"
        || run.trackingDurability !== "durable" || !Array.isArray(run.trackingProvenances)
        || !run.trackingProvenances.includes("launcherRegistered")
        || pathIdentity(run.runDir) !== pathIdentity(runDirectory)
        || pathIdentity(run.worktree) !== pathIdentity(request.root)
        || run.creatorPid !== request.creatorPid || run.startedAt !== request.started
        || run.detached !== request.detached) {
      throw new Error("registration error: host query does not confirm this exact durable run");
    }
  } catch (error) {
    if (error.message.startsWith("registration error:")) throw error;
    throw new Error("registration error: acknowledged run query is unavailable or unresolved; no tests started");
  } finally { clearTimeout(timeout); }
}

export async function registerTestRun(request, runDirectory, env = process.env) {
  if (!hasTermalContext(env)) {
    const registration = { state: "untracked", reason: "standalone invocation has no TermAl host/session environment" };
    console.log(`UNTRACKED ${request.runId}: ${registration.reason}`);
    return registration;
  }
  const configuration = hostConfiguration(env);
  const { session, url } = configuration;
  if (request.owner !== session) throw new Error("registration error: launching session differs from the run owner");
  // Both attempts send these exact bytes. A lost ACK cannot create another
  // run or change the immutable request while the outcome is unresolved.
  const body = JSON.stringify({ runId: request.runId, runDirectory });
  const identity = requestIdentity(request);
  const deadline = Date.now() + registrationBudgetMs;
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), Math.max(1, deadline - Date.now()));
    let definitiveRefusal = false;
    try {
      const response = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body,
        signal: controller.signal,
        redirect: "error",
      });
      if (!response.ok) {
        definitiveRefusal = true;
        await response.body?.cancel();
        throw new Error(`registration error: host refused registration (HTTP ${response.status})`);
      }
      // Once a reply arrives, malformed or unsupported content is a refusal;
      // only an ambiguous transport loss may retry these same POST bytes.
      const acknowledgement = await readResponse(response);
      definitiveRefusal = true;
      validateAcknowledgement(acknowledgement, request.runId);
      await queryRegisteredRun(request, runDirectory, configuration, deadline);
      if (requestIdentity(request) !== identity) throw new Error("registration error: immutable request changed during acknowledgement");
      console.log(`TRACKED ${request.runId}`);
      return { state: "registered", contract, version, runId: request.runId, registered: true, initiatingSessionId: session,
        hostOrigin: configuration.base.origin, runDirectory, requestIdentity: identity };
    } catch (error) {
      if (definitiveRefusal || error.message.startsWith("registration error:")) throw error;
      if (attempt === 1 || Date.now() >= deadline) {
        throw new Error("registration error: host acknowledgement is unavailable or unresolved; no tests started");
      }
    } finally {
      clearTimeout(timeout);
    }
  }
  throw new Error("registration error: host acknowledgement is unresolved; no tests started");
}

export async function validateRunRegistration(request, env, runDirectory) {
  if (request.registration?.state === "registered"
      && request.registration.runId === request.runId
      && request.registration.initiatingSessionId === request.owner) {
    const configuration = hostConfiguration(env);
    validateAcknowledgement(request.registration, request.runId);
    if (configuration.session === request.owner && request.registration.hostOrigin === configuration.base.origin
        && request.registration.requestIdentity === requestIdentity(request)
        && pathIdentity(request.registration.runDirectory) === pathIdentity(runDirectory)) {
      // Recheck visibility before worker preflight too: a replaced or rolled
      // back host cannot be bypassed by a local receipt from the parent.
      await queryRegisteredRun(request, runDirectory, configuration, Date.now() + registrationBudgetMs);
      return;
    }
  } else if (!hasTermalContext(env) && request.registration?.state === "untracked") {
    return;
  }
  throw new Error("registration error: this execution has no matching acknowledged registration");
}
