// Split from the launch boundary in scripts/test-launcher.mjs. Owns the
// bounded host registration handshake, not execution, credit or restart policy.
import { createHash, randomUUID } from "node:crypto";
import { realpathSync } from "node:fs";

const contextVariables = ["TERMAL_BASE_URL", "TERMAL_SESSION_ID", "TERMAL_CLI"];
const registrationBudgetMs = 8_000;
const responseByteLimit = 65_536;
const contract = "termal-durable-test-run-registration";
const version = 1;
const errorPrefix = "registration error:";
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
  if (!response.body) throw responseFailure("missing_response", "host response has no body");
  let length = 0;
  const chunks = [];
  for await (const chunk of response.body) {
    length += chunk.length;
    if (length > responseByteLimit) throw responseFailure("oversized_response", "oversized host response");
    chunks.push(chunk);
  }
  try { return JSON.parse(Buffer.concat(chunks).toString("utf8")); }
  catch { throw responseFailure("malformed_response", "malformed host response"); }
}

function responseFailure(category, message) {
  const error = new Error(`${errorPrefix} ${message}`);
  error.registrationCategory = category;
  return error;
}

function failureCategory(error, signal) {
  if (error?.registrationCategory) return error.registrationCategory;
  return signal?.aborted || error?.name === "AbortError" || error?.name === "TimeoutError"
    ? "aborted" : "transport_failure";
}

function transportErrorCode(error) {
  let cause = error;
  for (let depth = 0; cause && depth < 4; depth += 1, cause = cause.cause) {
    if (typeof cause.code === "string") return cause.code;
  }
}

function transportCode(error) {
  // Never copy exception messages, URLs, stacks or request/response bodies.
  const code = transportErrorCode(error);
  return code === undefined ? "unavailable" : code.replace(/[^A-Za-z0-9_-]/gu, "").slice(0, 80);
}

// Advisory timings only. The original Date.now deadline, exact request bytes,
// ACK predicates and retry decisions remain the execution boundary.
class RegistrationDiagnostics {
  constructor(request, deadline, operation = "registration") {
    this.started = performance.now();
    this.runId = request.runId;
    this.requestDigest = requestIdentity(request);
    this.ownerSessionId = request.owner;
    this.operationId = randomUUID();
    this.operation = operation;
    this.deadline = deadline;
    this.phaseStarted = this.started;
    this.phase = "operation_start";
    this.method = "POST";
    this.attempt = 1;
    this.status = "unavailable";
    this.spans = [];
    this.queryAttempt = 0;
    this.transportResets = 0;
  }

  enter(phase, status = this.status) {
    const now = performance.now();
    if (this.spans.length < 16) this.spans.push(`${this.phase}:${(this.phaseStarted - this.started).toFixed(1)}-${(now - this.started).toFixed(1)}`);
    this.phase = phase;
    this.method = phase.startsWith("get_") ? "GET" : "POST";
    this.status = status;
    this.phaseStarted = now;
  }

  fields(category) {
    const now = performance.now();
    return `phase=${this.phase}; category=${category}; runId=${this.runId}; attempt=${this.attempt}; httpStatus=${this.status}; `
      + `operationId=${this.operationId}; operation=${this.operation}; requestDigest=${this.requestDigest}; `
      + `elapsedMs=${(now - this.started).toFixed(1)}; phaseElapsedMs=${(now - this.phaseStarted).toFixed(1)}; `
      + `budgetMs=${registrationBudgetMs}; deadlineEpochMs=${this.deadline}; remainingMs=${Math.max(0, this.deadline - Date.now())}; `
      + `queryAttempt=${this.queryAttempt}; transportResets=${this.transportResets}; spans=${this.spans.join(",").slice(0, 224)}`;
  }

  failure(error, signal) {
    // A GET error must retain its own phase when crossing the POST catch.
    if (error?.registrationDiagnostic) return error;
    const category = error?.message?.startsWith(errorPrefix) && !error.registrationCategory
      && /_(?:identity|validation|immutable)$/u.test(this.phase)
      ? "identity_mismatch" : failureCategory(error, signal);
    const human = error?.message?.startsWith(errorPrefix)
      ? `${this.method} ${this.phase.endsWith("headers") ? "send" : "response"}: ${error.message.slice(errorPrefix.length).trim()}`
      : `${this.method} ${this.phase.endsWith("headers") ? "send" : "body"} ${category === "aborted" ? "aborted" : `transport failure (${transportCode(error)})`}; no tests started`;
    const result = new Error(`${errorPrefix} ${human}; ${this.fields(category)}`.slice(0, 1280), { cause: error });
    result.registrationDiagnostic = true;
    if (error?.durabilityPending) result.durabilityPending = error.durabilityPending;
    return result;
  }

  success() {
    console.log(`registration diagnostic: ${this.fields("confirmed")}`);
  }
}

// A pending reply is not a definite no-write refusal or an acknowledgement.
// Keep only this POST's validated identity and the supported wire fields;
// arbitrary response fields must not enter the error or retained artifact.
function registrationPending(body, response, diagnostics, reason) {
  if (response.status !== 503 || diagnostics.method !== "POST" || diagnostics.phase !== "post_body"
      || body?.contract !== "termal-durability-pending" || body.version !== 1
      || body.code !== "durability_pending" || body.outcome !== "pending"
      || body.operation !== "test_run_registration" || body.retry?.mode !== "same_request"
      || typeof body.error !== "string" || !body.error.trim()
      || typeof body.identity?.runId !== "string" || !body.identity.runId
      || body.identity?.runId !== diagnostics.runId
      || body.identity?.requestDigest !== diagnostics.requestDigest
      || body.identity?.ownerSessionId !== diagnostics.ownerSessionId) return;
  return {
    error: reason,
    contract: "termal-durability-pending", version: 1, code: "durability_pending", outcome: "pending",
    operation: "test_run_registration",
    identity: { runId: diagnostics.runId, requestDigest: diagnostics.requestDigest,
      ownerSessionId: diagnostics.ownerSessionId },
    retry: { mode: "same_request" },
  };
}

async function httpFailure(response, diagnostics, signal) {
  let reason = "host reason unavailable";
  let bodyCategory = "missing_reason";
  let bodyCode = "unavailable";
  let pending;
  try {
    const body = await readResponse(response);
    if (typeof body?.error === "string" && body.error.trim()) {
      reason = body.error.replace(/[\u0000-\u001f\u007f\u2028\u2029]/gu, " ").slice(0, 384);
      bodyCategory = "host_reason";
    }
    pending = registrationPending(body, response, diagnostics, reason);
  } catch (error) {
    // Non-2xx already forbids execution and automatic replay. A body failure
    // cannot turn that status into an ambiguous retryable POST transport loss.
    bodyCategory = failureCategory(error, signal);
    bodyCode = transportCode(error);
  }
  const disposition = pending ? "registration durability is pending; retry the unchanged original request; no tests started"
    : "host refused registration";
  const error = responseFailure(pending ? "durability_pending" : "http_error",
    `${diagnostics.method} HTTP ${response.status}: ${disposition}; ${reason}; errorBodyCategory=${bodyCategory}; errorBodyCode=${bodyCode}`);
  if (pending) error.durabilityPending = pending;
  return diagnostics.failure(error, signal);
}

function validateAcknowledgement(acknowledgement, runId) {
  if (!acknowledgement || acknowledgement.contract !== contract || acknowledgement.version !== version
      || acknowledgement.runId !== runId || acknowledgement.registered !== true) {
    const category = !acknowledgement || typeof acknowledgement !== "object" ? "malformed_acknowledgement"
      : acknowledgement.contract !== contract || acknowledgement.version !== version ? "unsupported_contract"
        : acknowledgement.runId !== runId ? "identity_mismatch" : "disposition_mismatch";
    throw responseFailure(category, "host did not acknowledge this exact run with the supported durable contract");
  }
}

async function queryRegisteredRun(request, runDirectory, configuration, deadline, diagnostics) {
  diagnostics.enter("get_headers", "unavailable");
  const remaining = deadline - Date.now();
  if (remaining <= 0) throw diagnostics.failure(responseFailure("aborted", "GET query budget exhausted; no tests started"));
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), remaining);
  try {
    const url = new URL(`/api/test-runs/${encodeURIComponent(request.runId)}`, configuration.base);
    const options = {
      signal: controller.signal, redirect: "error",
      headers: { "x-termal-registration-operation": diagnostics.operationId, "x-termal-registration-attempt": String(diagnostics.attempt) },
    };
    let response;
    // This is an idempotent exact read, not a new registration. A reset before
    // fetch delivers a Response permits one reread under the original budget.
    // It does not prove a stale pooled socket or that no wire bytes arrived.
    for (let attempt = 1; attempt <= 2; attempt += 1) {
      diagnostics.queryAttempt = attempt;
      if (attempt > 1) diagnostics.enter("get_headers", "unavailable");
      if (controller.signal.aborted || Date.now() >= deadline) {
        throw responseFailure("aborted", "GET query budget exhausted; no tests started");
      }
      try {
        response = await fetch(url, options);
        break;
      } catch (error) {
        const reset = failureCategory(error, controller.signal) === "transport_failure"
          && transportErrorCode(error) === "ECONNRESET";
        if (reset) diagnostics.transportResets += 1;
        if (attempt !== 1 || !reset || controller.signal.aborted || Date.now() >= deadline) throw error;
        diagnostics.enter("get_reset", "unavailable");
      }
    }
    // Once a Response exists, status, body and identity failures are terminal.
    diagnostics.enter("get_body", response.status);
    if (!response.ok) {
      throw await httpFailure(response, diagnostics, controller.signal);
    }
    const { run } = await readResponse(response);
    diagnostics.enter("get_identity");
    if (!run || run.runId !== request.runId || run.ownerSessionId !== configuration.session
        || run.initiatingSessionId !== configuration.session || run.registrationState !== "registered"
        || run.trackingDurability !== "durable" || !Array.isArray(run.trackingProvenances)
        || !run.trackingProvenances.includes("launcherRegistered")
        || pathIdentity(run.runDir) !== pathIdentity(runDirectory)
        || pathIdentity(run.worktree) !== pathIdentity(request.root)
        || run.creatorPid !== request.creatorPid || run.startedAt !== request.started
        || run.detached !== request.detached) {
      const category = run?.runId !== request.runId ? "identity_mismatch"
        : run.registrationState !== "registered" ? "disposition_mismatch"
          : run.trackingDurability !== "durable" ? "durability_unconfirmed" : "identity_mismatch";
      throw responseFailure(category, "host query does not confirm this exact durable run");
    }
  } catch (error) {
    throw diagnostics.failure(error, controller.signal);
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
  const diagnostics = new RegistrationDiagnostics(request, deadline);
  for (let attempt = 0; attempt < 2; attempt += 1) {
    diagnostics.attempt = attempt + 1;
    diagnostics.enter("post_headers", "unavailable");
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), Math.max(1, deadline - Date.now()));
    let definitiveRefusal = false;
    try {
      const response = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json", "x-termal-registration-operation": diagnostics.operationId,
          "x-termal-registration-attempt": String(diagnostics.attempt) },
        body,
        signal: controller.signal,
        redirect: "error",
      });
      diagnostics.enter("post_body", response.status);
      if (!response.ok) {
        definitiveRefusal = true;
        throw await httpFailure(response, diagnostics, controller.signal);
      }
      // Once a reply arrives, malformed or unsupported content is a refusal;
      // only an ambiguous transport loss may retry these same POST bytes.
      const acknowledgement = await readResponse(response);
      definitiveRefusal = true;
      diagnostics.enter("post_validation");
      validateAcknowledgement(acknowledgement, request.runId);
      await queryRegisteredRun(request, runDirectory, configuration, deadline, diagnostics);
      diagnostics.enter("post_immutable");
      if (requestIdentity(request) !== identity) throw new Error("registration error: immutable request changed during acknowledgement");
      diagnostics.success();
      console.log(`TRACKED ${request.runId}`);
      return { state: "registered", contract, version, runId: request.runId, registered: true, initiatingSessionId: session,
        hostOrigin: configuration.base.origin, runDirectory, requestIdentity: identity };
    } catch (error) {
      if (definitiveRefusal || error.message.startsWith("registration error:")) {
        throw diagnostics.failure(error, controller.signal);
      }
      if (attempt === 1 || Date.now() >= deadline) {
        throw diagnostics.failure(error, controller.signal);
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
      const deadline = Date.now() + registrationBudgetMs;
      const diagnostics = new RegistrationDiagnostics(request, deadline, "preflight_confirmation");
      await queryRegisteredRun(request, runDirectory, configuration, deadline, diagnostics);
      diagnostics.success();
      return;
    }
  } else if (!hasTermalContext(env) && request.registration?.state === "untracked") {
    return;
  }
  throw new Error("registration error: this execution has no matching acknowledged registration");
}
