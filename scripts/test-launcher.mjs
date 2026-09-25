#!/usr/bin/env node
// One execution, durable logs, one completion. Notification retries never run tests.
import { spawn, spawnSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import {
  constants,
  accessSync,
  closeSync,
  createReadStream,
  existsSync,
  linkSync,
  mkdirSync,
  openSync,
  readFileSync,
  renameSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, delimiter, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import {
  captureFingerprint,
  fingerprintComponentNames,
  isolatedGitEnvironment,
} from "./review-freeze-fingerprint.mjs";
import { processMayBeAlive } from "./test-temp-root.mjs";

const script = fileURLToPath(import.meta.url);
const repository = resolve(dirname(script), "..");
const diagnosticLimit = 2400;
const readJson = (path) => JSON.parse(readFileSync(path, "utf8"));
export const helperTestFiles = Object.freeze([
  "scripts/review-freeze-fingerprint.test.mjs",
  "scripts/test-launcher.test.mjs",
  "scripts/test-temp-root.test.mjs",
  "scripts/vitest-resource-preflight.test.mjs",
]);
const helperSupportFiles = Object.freeze([
  "scripts/review-freeze-fingerprint.mjs",
  "scripts/test-launcher.mjs",
  "scripts/test-temp-root.mjs",
  "scripts/vitest-resource-preflight.mjs",
]);

function save(path, value) {
  const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
  try {
    writeFileSync(temporary, `${JSON.stringify(value, null, 2)}\n`, { flag: "wx" });
    renameSync(temporary, path);
  } finally {
    try {
      unlinkSync(temporary);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
}

// A result is terminal once it records how the run ended; anything else is
// UNKNOWN (running or interrupted), never a pass.
function isTerminal(result) {
  return ["passed", "failed"].includes(result?.state) &&
    Boolean(result.ended) && Number.isInteger(result.exitCode);
}

function failureInvestigation(runDir) {
  return {
    status: "required",
    runDir,
    action: "Preserve this failed run, state falsifiable hypotheses, use focused discriminating diagnostics, classify the root cause, and fix confirmed test or runner defects without another approval round trip.",
    retryPolicy: "A passing retry is not a diagnosis or closure. Do not auto-retry, weaken or ignore tests, inflate timeouts, or change product semantics without authority.",
  };
}

function attachFailureInvestigation(result, runDir) {
  if (result.state === "failed" && !result.failureInvestigation) {
    result.failureInvestigation = failureInvestigation(runDir);
  }
}

function windowsGitBash(env) {
  if (env.TERMAL_TEST_GIT_BASH) return env.TERMAL_TEST_GIT_BASH;
  return join(env.ProgramFiles ?? "C:\\Program Files", "Git", "bin", "bash.exe");
}

export function requiredStages(platform = process.platform, env = process.env) {
  const rustCommand = platform === "win32" ? windowsGitBash(env) : "sh";
  const rustArgs = platform === "win32"
    ? ["-c", "exec scripts/test-rust.sh"]
    : ["scripts/test-rust.sh"];
  return [
    { name: "cargo-check", command: "cargo", args: ["check"] },
    {
      name: "typescript",
      command: process.execPath,
      args: ["node_modules/typescript/bin/tsc", "--noEmit"],
      cwd: "ui",
    },
    {
      name: "fingerprint-tests",
      command: process.execPath,
      args: ["--test", ...helperTestFiles],
    },
    { name: "rust-tests", command: rustCommand, args: rustArgs },
    {
      name: "vitest",
      command: process.execPath,
      args: ["node_modules/vitest/vitest.mjs", "run"],
      cwd: "ui",
    },
  ];
}

export function requiredFiles(root = repository) {
  return [
    join(root, "scripts", "test-rust.sh"),
    ...[...helperTestFiles, ...helperSupportFiles].map((path) => join(root, path)),
    join(root, "ui", "node_modules", "typescript", "bin", "tsc"),
    join(root, "ui", "node_modules", "vitest", "vitest.mjs"),
  ];
}

export function liveStage(platform = process.platform, env = process.env) {
  const command = platform === "win32" ? windowsGitBash(env) : "sh";
  const liveArgs = [
    "--bin",
    "termal",
    "root_recovery_live",
    "--",
    "--ignored",
    "--test-threads=1",
  ];
  return {
    name: "engram-live",
    command,
    args: platform === "win32"
      ? ["-c", `exec scripts/test-rust.sh ${liveArgs.join(" ")}`]
      : ["scripts/test-rust.sh", ...liveArgs],
  };
}

function executable(command, cwd, env) {
  const path = Object.entries(env).find(([key]) => key.toLowerCase() === "path")?.[1] ?? "";
  const bases = /[\\/]/u.test(command) || isAbsolute(command)
    ? [resolve(cwd, command)]
    : path.split(delimiter).filter(Boolean).map((part) => join(part, command));
  // .cmd/.bat require a shell: callers must name that shell explicitly.
  for (const base of bases) {
    for (const candidate of process.platform === "win32" ? [base, `${base}.exe`] : [base]) {
      try {
        if (process.platform === "win32" && !/\.exe$/iu.test(candidate)) continue;
        if (!statSync(candidate).isFile()) continue;
        accessSync(candidate, process.platform === "win32" ? constants.F_OK : constants.X_OK);
        return candidate;
      } catch {
        // Try the next PATH entry.
      }
    }
  }
  throw new Error(`executable not found: ${command}; check PATH or supply the shell explicitly`);
}

function requiredFile(path) {
  const metadata = statSync(path);
  if (!metadata.isFile()) throw new Error(`required file is not a file: ${path}`);
}

function stageDirectory(root, cwd) {
  if (cwd !== undefined && typeof cwd !== "string") {
    throw new Error("stage cwd must be a repository-relative string");
  }
  const directory = resolve(root, cwd ?? ".");
  const fromRoot = relative(root, directory);
  if (fromRoot === ".." || fromRoot.startsWith(`..${sep}`) || isAbsolute(fromRoot)) {
    throw new Error(`stage cwd escapes repository root: ${cwd}`);
  }
  if (!statSync(directory).isDirectory()) {
    throw new Error(`stage cwd is not a directory: ${directory}`);
  }
  return directory;
}

async function sha256(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

export async function createRun({
  root = repository,
  stages,
  notifyTo,
  requiredBinaryEnv = [],
  prerequisiteFiles = [],
  needsCargo = false,
  liveEngram,
  full = false,
  detached = false,
}, env = process.env) {
  const normalizedRoot = resolve(root);
  const gitEnv = isolatedGitEnvironment(env);
  const notification = notifyTo === undefined
    ? undefined
    : validateNotification({ notifyTo, owner: env.TERMAL_SESSION_ID, root: normalizedRoot }, env);
  const git = spawnSync(
    "git",
    ["rev-parse", "--path-format=absolute", "--git-path", "review-runs"],
    { cwd: normalizedRoot, env: gitEnv, encoding: "utf8", windowsHide: true },
  );
  if (git.error || git.status !== 0) {
    throw new Error(`cannot locate Git run directory: ${git.error?.message ?? git.stderr}`);
  }
  const runId = `test-${randomUUID()}`;
  const runDir = join(git.stdout.trim(), runId);
  mkdirSync(runDir, { recursive: true });
  const request = {
    runId,
    root: normalizedRoot,
    stages,
    notifyTo: notification?.notifyTo,
    requiredBinaryEnv,
    prerequisiteFiles,
    needsCargo,
    liveEngram,
    full,
    // Read by TermAl's run index (docs/features/test-runs.md): whether the
    // stages run in a detached worker, and the process that stands for the
    // run until a worker records its own pid in results.json.
    detached,
    creatorPid: process.pid,
    owner: notification?.owner ?? env.TERMAL_SESSION_ID ?? null,
    started: new Date().toISOString(),
  };
  save(join(runDir, "request.json"), request);
  save(join(runDir, "results.json"), {
    runId,
    state: "running",
    started: request.started,
    stages: (stages ?? []).map(({ name }) => ({ name, state: "unrun" })),
  });
  try {
    const input = await captureFingerprint(normalizedRoot, gitEnv);
    save(join(runDir, "input.json"), input);
    request.expectedFingerprint = input.fingerprint;
    save(join(runDir, "request.json"), request);
  } catch (error) {
    const result = readJson(join(runDir, "results.json"));
    Object.assign(result, {
      state: "failed",
      exitCode: 1,
      ended: new Date().toISOString(),
      error: error.message,
    });
    attachFailureInvestigation(result, runDir);
    save(join(runDir, "results.json"), result);
  }
  return runDir;
}

export async function runCommand(command, args, { cwd, env, log }) {
  const fd = openSync(log, "wx");
  try {
    return await new Promise((done) => {
      let error;
      const child = spawn(command, args, {
        cwd,
        env,
        windowsHide: true,
        stdio: ["ignore", fd, fd],
      });
      child.on("error", (value) => { error = value.message; });
      child.on("close", (code, signal) => done({ code, signal, ...(error ? { error } : {}) }));
    });
  } finally {
    closeSync(fd);
  }
}

// Inspect logs once after close, never tail them. Full bytes stay on disk.
// Filtering never determines success.
export async function diagnostics(log, failed) {
  let selected = "";
  let tail = "";
  let pending = "";
  let matchingBytes = 0;
  let longLine = false;
  let continuation = false;
  let context = 0;
  const accept = (line) => {
    const clean = line.replace(/\x1b\[[0-9;]*[A-Za-z]/gu, "");
    if (/^\s*(?:ok\s+\d+\b|# Subtest:|test\s+.*\.\.\.\s+ok\b|test result: ok\.|[✔✓])/u.test(clean)) {
      return;
    }
    tail = `${tail}${clean}\n`.slice(-diagnosticLimit);
    const nodeWarning = /^\s*(?:#\s*)?\(node:\d+\)\s+(?:\[[^\]\r\n]+\]\s+)?[A-Za-z]*Warning:/u.test(clean);
    const match = nodeWarning || /\b(?:warning|error)(?:\b|\[)|^\s*(?:test .*\.\.\. FAILED|test result: FAILED|FAIL\s|not ok\b|✖|failures:|thread .*panicked|Caused by:)/iu.test(clean);
    if (match) context = 5;
    if (context > 0) {
      const text = `${clean}\n`;
      matchingBytes += text.length;
      selected += text.slice(0, Math.max(0, diagnosticLimit - selected.length));
      context -= 1;
    }
  };
  for await (const chunk of createReadStream(log, { encoding: "utf8", highWaterMark: 8192 })) {
    pending += chunk;
    let end;
    while ((end = pending.indexOf("\n")) >= 0) {
      if (!continuation) accept(pending.slice(0, end));
      continuation = false;
      pending = pending.slice(end + 1);
    }
    if (pending.length > 16384) {
      // Retain one bounded prefix per logical line. Never reinterpret a later
      // fragment as a passing-test header or a new failure/context line.
      if (!continuation) accept(pending.slice(0, 16384));
      pending = "";
      continuation = true;
      longLine = true;
    }
  }
  if (pending && !continuation) accept(pending);
  const fallback = failed && !selected;
  return {
    text: selected || (fallback ? tail : ""),
    truncated: longLine || matchingBytes > diagnosticLimit ||
      (fallback && statSync(log).size > diagnosticLimit),
    ...(fallback ? { fallback: "no recognized diagnostic; bounded failure tail" } : {}),
  };
}

function changedFingerprintComponents(expected, observed) {
  const changed = fingerprintComponentNames.filter(
    (name) => expected?.[name] !== observed?.[name],
  );
  return changed.length > 0 ? changed : ["compositeFingerprint"];
}

function driftError(prefix, expected, observed) {
  return `${prefix}; changed fingerprint components: ${changedFingerprintComponents(expected, observed).join(", ")}`;
}

function normalizedSessionAddress(value, label) {
  if (typeof value !== "string") throw new Error(`${label} is missing`);
  const normalized = value.trim();
  if (!normalized || normalized.startsWith("-")) {
    throw new Error(`${label} must be nonempty and must not look like an option`);
  }
  return normalized;
}

function validateNotification(request, env) {
  const notifyTo = normalizedSessionAddress(request.notifyTo, "notification target");
  const owner = normalizedSessionAddress(request.owner, "notification sender");
  if (normalizedSessionAddress(env.TERMAL_SESSION_ID, "inherited TERMAL_SESSION_ID") !== owner) {
    throw new Error("notification requires the original inherited TERMAL_SESSION_ID; never impersonate");
  }
  if (notifyTo === owner) {
    throw new Error("self-send is not supported; use a genuine worker and coordinator, or foreground host completion wait");
  }
  if (!env.TERMAL_CLI || !isAbsolute(env.TERMAL_CLI)) {
    throw new Error("TERMAL_CLI must name the inherited absolute executable");
  }
  return {
    cli: executable(env.TERMAL_CLI, request.root, env),
    notifyTo,
    owner,
  };
}

function validateInitialRequest(runDir, request, env) {
  if (!request || typeof request !== "object" || request.runId !== basename(runDir) ||
      !isAbsolute(request.root)) {
    throw new Error("invalid worker request identity or repository root");
  }
  if (!Array.isArray(request.stages) || request.stages.length === 0) {
    throw new Error("at least one stage is required");
  }
  const names = new Set();
  for (const stage of request.stages) {
    if (!stage || typeof stage !== "object" ||
        !/^[a-zA-Z0-9_-]+$/u.test(stage.name) || names.has(stage.name) ||
        typeof stage.command !== "string" || !Array.isArray(stage.args) ||
        !stage.args.every((arg) => typeof arg === "string")) {
      throw new Error("worker request contains an invalid stage");
    }
    names.add(stage.name);
  }
  if (request.notifyTo !== undefined) validateNotification(request, env);
}

function validateInitialResult(request, result) {
  if (!result || typeof result !== "object" || result.runId !== request.runId ||
      !["running", "failed"].includes(result.state) || !Array.isArray(result.stages) ||
      result.stages.length !== request.stages.length ||
      result.stages.some((stage, index) => stage?.name !== request.stages[index].name)) {
    throw new Error("invalid initial worker results");
  }
  if (result.state === "running" && !/^[a-f0-9]{64}$/u.test(request.expectedFingerprint ?? "")) {
    throw new Error("worker request has no valid source fingerprint");
  }
}

function startupFailureResult(runDir, request, result, error, notificationEligible) {
  const stages = Array.isArray(request?.stages)
    ? request.stages
      .filter((stage) => stage && typeof stage.name === "string")
      .map(({ name }) => ({ name, state: "unrun" }))
    : [];
  const failed = {
    runId: basename(runDir),
    state: "failed",
    started: typeof result?.started === "string"
      ? result.started
      : typeof request?.started === "string" ? request.started : new Date().toISOString(),
    stages,
    exitCode: 1,
    ended: new Date().toISOString(),
    error: `worker startup failed before readiness: ${error.message}`,
    notificationEligible,
  };
  attachFailureInvestigation(failed, runDir);
  return failed;
}

export async function executeRun(runDir, env = process.env, { onReady } = {}) {
  // Exclusive admission: an interrupted run is not a retryable test command.
  closeSync(openSync(join(runDir, "execution.lock"), "wx"));
  const resultPath = join(runDir, "results.json");
  let request;
  let result;
  let requestTrusted = false;
  try {
    request = readJson(join(runDir, "request.json"));
    validateInitialRequest(runDir, request, env);
    requestTrusted = true;
    result = readJson(resultPath);
    validateInitialResult(request, result);
  } catch (error) {
    const failed = startupFailureResult(
      runDir,
      request,
      result,
      error,
      requestTrusted && request.notifyTo !== undefined,
    );
    save(resultPath, failed);
    return failed;
  }
  if (result.state === "failed") {
    try {
      await onReady?.();
    } catch (error) {
      result.error = `${result.error ? `${result.error}; ` : ""}detached readiness failed: ${error.message}`;
      attachFailureInvestigation(result, runDir);
      save(resultPath, result);
    }
    return result;
  }
  const childEnv = { ...env };
  const probe = async (name, command, args) => {
    const log = join(runDir, `preflight-${name}.log`);
    const outcome = await runCommand(command, args, {
      cwd: request.root,
      env: childEnv,
      log,
    });
    result.preflight.push({
      name,
      command: [command, ...args],
      cwd: request.root,
      ...outcome,
      log,
      diagnostics: await diagnostics(log, outcome.code !== 0 || Boolean(outcome.error)),
    });
    save(resultPath, result);
    if (outcome.code !== 0 || outcome.error) {
      throw new Error(`${name} preflight failed (${outcome.error ?? outcome.code}); see ${log}`);
    }
  };
  try {
    result.pid = process.pid;
    result.owner = request.owner;
    result.preflight = [];
    save(resultPath, result);
    await onReady?.();
    for (const path of request.prerequisiteFiles ?? []) requiredFile(path);
    let cargo;
    if (request.needsCargo) {
      const explicitCargo = typeof env.TERMAL_TEST_CARGO === "string" &&
        env.TERMAL_TEST_CARGO.trim() ? env.TERMAL_TEST_CARGO.trim() : undefined;
      cargo = executable(explicitCargo ?? "cargo", request.root, childEnv);
      childEnv.TERMAL_TEST_CARGO = cargo.replaceAll("\\", "/");
      result.cargo = {
        path: cargo,
        source: explicitCargo ? "TERMAL_TEST_CARGO" : "PATH",
      };
      save(resultPath, result);
    }
    const names = new Set();
    for (const stage of request.stages) {
      if (!/^[a-zA-Z0-9_-]+$/u.test(stage.name) || names.has(stage.name)) {
        throw new Error("stage names must be unique safe filenames");
      }
      names.add(stage.name);
      if (typeof stage.command !== "string" || !Array.isArray(stage.args) ||
          !stage.args.every((arg) => typeof arg === "string")) {
        throw new Error("stage must contain a command and string arguments");
      }
      stage.cwd = stageDirectory(request.root, stage.cwd);
      stage.command = request.needsCargo && stage.name === "cargo-check"
        ? cargo
        : executable(stage.command, stage.cwd, childEnv);
    }
    if (request.notifyTo) validateNotification(request, env);
    for (const name of request.requiredBinaryEnv ?? []) {
      if (!/^[A-Z][A-Z0-9_]*$/u.test(name)) {
        throw new Error("invalid binary environment variable name");
      }
      if (!env[name] || !isAbsolute(env[name])) {
        throw new Error(`required binary environment variable ${name} must be an absolute executable path`);
      }
      let binary;
      try {
        binary = executable(env[name], request.root, childEnv);
      } catch (error) {
        throw new Error(`${name}: ${error.message}`);
      }
      await probe(name, binary, ["--version"]);
    }
    if (request.needsCargo) {
      await probe("cargo", cargo, ["--version"]);
      const shell = request.stages.find((stage) => ["rust-tests", "engram-live"].includes(stage.name));
      if (shell && shell.command !== cargo) {
        await probe("test-shell", shell.command, ["-c", "exit 0"]);
      }
    }
    if (request.liveEngram) {
      const { path, expectedSha256 } = request.liveEngram;
      if (!isAbsolute(path) || !/^[a-f0-9]{64}$/iu.test(expectedSha256)) {
        throw new Error("live Engram requires an absolute binary and a SHA-256 fingerprint");
      }
      const binary = executable(path, request.root, childEnv);
      const observedSha256 = await sha256(binary);
      result.liveEngram = { path: binary, expectedSha256, observedSha256 };
      save(resultPath, result);
      if (observedSha256.toLowerCase() !== expectedSha256.toLowerCase()) {
        throw new Error(`live Engram SHA-256 mismatch: expected ${expectedSha256}, got ${observedSha256}`);
      }
      childEnv.TERMAL_TEST_LIVE_ENGRAM_BINARY = binary;
      childEnv.TERMAL_TEST_LIVE_ENGRAM_SHA256 = expectedSha256;
      await probe("engram", binary, ["--version"]);
    }
    const expectedInput = readJson(join(runDir, "input.json"));
    const before = await captureFingerprint(request.root, childEnv);
    if (before.fingerprint !== request.expectedFingerprint) {
      throw new Error(driftError("input drift before execution; no stages run", expectedInput, before));
    }
    result.expectedFingerprint = request.expectedFingerprint;
    result.before = before.fingerprint;
    if (process.platform === "win32") {
      result.limitations = "Windows: untracked executable modes and filesystem symlink properties are unverified.";
    }
    save(resultPath, result);
    for (let index = 0; index < request.stages.length; index += 1) {
      const stage = request.stages[index];
      const entry = result.stages[index];
      Object.assign(entry, {
        state: "running",
        started: new Date().toISOString(),
        command: [stage.command, ...stage.args],
        cwd: stage.cwd,
        log: join(runDir, `${stage.name}.log`),
      });
      save(resultPath, result);
      const outcome = await runCommand(stage.command, stage.args, {
        cwd: stage.cwd,
        env: childEnv,
        log: entry.log,
      });
      Object.assign(entry, outcome, {
        ended: new Date().toISOString(),
        state: outcome.code === 0 && !outcome.error ? "passed" : "failed",
      });
      entry.diagnostics = await diagnostics(entry.log, entry.state === "failed");
      save(resultPath, result);
      if (entry.state === "failed") break;
    }
    const after = await captureFingerprint(request.root, childEnv);
    result.after = after.fingerprint;
    if (result.after !== result.expectedFingerprint) {
      throw new Error(driftError("input drift: results do not validate the current source", expectedInput, after));
    }
    result.state = result.stages.every((stage) => stage.state === "passed") ? "passed" : "failed";
    result.exitCode = result.stages.find((stage) => stage.state === "failed")?.code ??
      (result.state === "passed" ? 0 : 1);
  } catch (error) {
    result.state = "failed";
    result.exitCode = 1;
    result.error = error.message;
    for (const stage of result.stages) {
      if (stage.state === "running") {
        stage.state = "failed";
        stage.error = error.message;
      }
    }
  }
  attachFailureInvestigation(result, runDir);
  result.ended = new Date().toISOString();
  save(resultPath, result);
  return result;
}

export async function summarize(runDir) {
  const result = readJson(join(runDir, "results.json"));
  const terminal = isTerminal(result);
  const lines = [
    `${terminal ? result.state === "passed" ? "PASS" : "FAIL" : "UNKNOWN (no terminal result; running or interrupted)"} ${result.runId} exit=${terminal ? result.exitCode : "unknown"}`,
    `results: ${join(runDir, "results.json")}`,
  ];
  const investigation = result.failureInvestigation ??
    (terminal && result.state === "failed" ? failureInvestigation(runDir) : undefined);
  if (investigation) {
    lines.push(`INVESTIGATION REQUIRED: ${investigation.action}`);
    lines.push(investigation.retryPolicy);
  }
  if (result.error) lines.push(`runner: ${result.error.slice(0, diagnosticLimit)}`);
  let remaining = 6000;
  // Failure excerpts get first use of the shared budget; earlier successful
  // commands may emit warnings but must not crowd out the actual failure.
  const entries = [
    ...(result.preflight ?? []).filter((probe) => probe.diagnostics?.text).map((probe) => ({
      ...probe,
      failed: probe.code !== 0 || Boolean(probe.error),
      header: `preflight ${probe.name}: exit=${probe.code} log=${probe.log}`,
    })),
    ...result.stages.map((stage) => ({
      ...stage,
      failed: stage.state === "failed",
      header: `${stage.name}: ${stage.state} exit=${stage.code ?? "unrun/unknown"}${stage.log ? ` log=${stage.log}` : ""}`,
    })),
  ].sort((left, right) => Number(right.failed) - Number(left.failed));
  for (const stage of entries) {
    lines.push(stage.header);
    if (stage.error) lines.push(stage.error.slice(0, diagnosticLimit));
    if (stage.diagnostics?.text) {
      const text = stage.diagnostics.text.trimEnd();
      if (remaining > 0) lines.push(text.slice(0, remaining));
      if (text.length > remaining) lines.push("[summary diagnostics truncated; full output in log]");
      remaining = Math.max(0, remaining - text.length);
    }
    if (stage.diagnostics?.truncated) lines.push("[diagnostics truncated; full output in log]");
  }
  if (result.limitations) lines.push(result.limitations);
  return `${lines.join("\n")}\n`;
}

export async function notifyRun(runDir, env = process.env, send = runCommand) {
  const request = readJson(join(runDir, "request.json"));
  const result = readJson(join(runDir, "results.json"));
  if (!isTerminal(result)) {
    throw new Error("cannot notify before terminal results are saved");
  }
  if (result.notificationEligible === false) {
    throw new Error("this run's request failed validation, so no completion may be sent for it");
  }
  const { cli, notifyTo } = validateNotification(request, env);
  const messageFile = join(runDir, "notification.message.txt");
  if (!existsSync(messageFile)) {
    const temporaryMessageFile = `${messageFile}.${process.pid}.${randomUUID()}.tmp`;
    try {
      writeFileSync(temporaryMessageFile, await summarize(runDir), { flag: "wx" });
      try {
        linkSync(temporaryMessageFile, messageFile);
      } catch (error) {
        if (error.code !== "EEXIST") throw error;
      }
    } finally {
      try {
        unlinkSync(temporaryMessageFile);
      } catch (error) {
        if (error.code !== "ENOENT") throw error;
      }
    }
  }
  const args = [
    "mailbox",
    "send",
    "--to",
    notifyTo,
    "--message-file",
    messageFile,
    "--idempotency-key",
    `termal-tests:${request.runId}`,
    "--json",
  ];
  const log = join(runDir, `notification-${randomUUID()}.log`);
  const outcome = await send(cli, args, { cwd: request.root, env, log });
  const receipt = { ...outcome, log, attempted: new Date().toISOString() };
  save(join(runDir, `notification-receipt-${randomUUID()}.json`), receipt);
  try {
    save(join(runDir, "notification.json"), receipt);
  } catch (error) {
    if (!["EACCES", "EPERM"].includes(error.code)) throw error;
    const concurrent = readJson(join(runDir, "notification.json"));
    if (!concurrent || typeof concurrent !== "object" ||
        typeof concurrent.attempted !== "string" || typeof concurrent.log !== "string") {
      throw error;
    }
  }
  if (outcome.code !== 0 || outcome.error) {
    throw new Error(`notification failed; tests were NOT rerun. Retry: node scripts/test-launcher.mjs notify "${runDir}"; log=${log}`);
  }
  return receipt;
}

function notificationDelivered(runDir) {
  const path = join(runDir, "notification.json");
  if (!existsSync(path)) return false;
  const receipt = readJson(path);
  return receipt?.code === 0 && !receipt.error;
}

// One best-effort message from an admitted runner that could not save a
// terminal result, so its coordinator is not left waiting. It says UNKNOWN,
// never PASS, and uses its own idempotency key: the run's completion key stays
// free for the genuine result a later `recover` sends.
export async function notifyRunnerFailure(runDir, error, env = process.env, send = runCommand) {
  const request = readJson(join(runDir, "request.json"));
  if (request.notifyTo === undefined) return undefined;
  const { cli, notifyTo } = validateNotification(request, env);
  const messageFile = join(runDir, "notification.runner-error.txt");
  writeFileSync(messageFile, [
    `UNKNOWN ${request.runId}: the runner could not save a terminal result: ${error.message}`.slice(0, diagnosticLimit),
    "Nothing here is a PASS, and no stage was rerun.",
    `Inspect: node scripts/test-launcher.mjs summary "${runDir}"`,
    `Once the worker has exited, settle the run: node scripts/test-launcher.mjs recover "${runDir}"`,
    "If results.json itself cannot be written, recover fails the same way and the run stays UNKNOWN.",
    "",
  ].join("\n"));
  const args = [
    "mailbox",
    "send",
    "--to",
    notifyTo,
    "--message-file",
    messageFile,
    "--idempotency-key",
    `termal-tests:${request.runId}:runner-error`,
    "--json",
  ];
  return send(cli, args, {
    cwd: request.root,
    env,
    log: join(runDir, `notification-runner-error-${randomUUID()}.log`),
  });
}

// Settles a run whose worker is gone, once, on request: nothing watches or
// polls for it. It never runs a stage. It refuses while the recorded worker
// may be alive (only proof of its death counts), and for a run no process ever
// took ownership of: no stage ran under it. A settled run is failed and marked
// interrupted; a stage that was running has an unknown outcome, never a pass.
// `recovery.lock` serialises settlement and is released after every attempt,
// so a failed write can be retried; the result is read again under it, so a
// terminal result the worker saved meanwhile is kept, not overwritten. The
// completion then goes out as `notify` would send it, under the run's own key,
// and only from the session that owns the run.
export async function recoverRun(runDir, env = process.env, {
  send = runCommand,
  isAlive = processMayBeAlive,
  saveResult = save,
} = {}) {
  const resultPath = join(runDir, "results.json");
  const request = readJson(join(runDir, "request.json"));
  const readResult = () => {
    const current = readJson(resultPath);
    if (!current || typeof current !== "object" || current.runId !== basename(runDir) ||
        !Array.isArray(current.stages)) {
      throw new Error(`results.json does not describe run ${basename(runDir)}`);
    }
    return current;
  };
  let result = readResult();
  let settled = false;
  if (!isTerminal(result)) {
    if (!Number.isSafeInteger(result.pid) || result.pid <= 0) {
      throw new Error("no process ever took ownership of this run (it never started, or died before admission), so no stage ran under it; inspect it with summary");
    }
    if (isAlive(result.pid)) {
      throw new Error(`worker pid ${result.pid} may still be running; wait for its completion, or see docs/test.md if that pid now belongs to another process`);
    }
    const lock = join(runDir, "recovery.lock");
    let locked = false;
    try {
      closeSync(openSync(lock, "wx"));
      locked = true;
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
    }
    try {
      // Read again under the lock: the worker, or a concurrent recovery,
      // may have saved a terminal result since the first read.
      result = readResult();
      if (!isTerminal(result) && !locked) {
        throw new Error(`another recovery of this run is in progress, or one was killed before releasing ${lock}`);
      }
      if (!isTerminal(result)) {
        const interruption = `worker ${result.pid} exited without saving a terminal result`;
        for (const stage of result.stages) {
          if (stage.state === "running") {
            stage.state = "failed";
            stage.error = `interrupted: ${interruption}; this stage's outcome is unknown`;
          }
        }
        Object.assign(result, {
          state: "failed",
          exitCode: 1,
          interrupted: true,
          error: `interrupted: ${interruption}; no stage was rerun`,
          ended: new Date().toISOString(),
        });
        attachFailureInvestigation(result, runDir);
        saveResult(resultPath, result);
        settled = true;
      }
    } finally {
      if (locked) {
        try {
          unlinkSync(lock);
        } catch (error) {
          if (error.code !== "ENOENT") throw error;
        }
      }
    }
  }
  let notification = "not requested";
  if (request.notifyTo !== undefined) {
    if (result.notificationEligible === false) {
      notification = "not sent: the run's request failed validation";
    } else if (notificationDelivered(runDir)) notification = "already delivered";
    else if (env.TERMAL_SESSION_ID?.trim() !== request.owner) {
      notification = "not sent: only the session that owns the run may send its completion";
    } else {
      await notifyRun(runDir, env, send);
      notification = "sent";
    }
  }
  return { settled, notification };
}

async function sendWorkerReady(runDir) {
  if (typeof process.send !== "function") {
    throw new Error("detached worker has no readiness IPC channel");
  }
  await new Promise((resolveReady, rejectReady) => {
    process.send({ type: "READY", runDir: resolve(runDir) }, (error) => {
      if (error) rejectReady(error);
      else resolveReady();
    });
  });
}

async function finish(runDir, { workerHandshake = false } = {}) {
  // Admission is the point after which this process owns the run: a failure
  // before it (another owner's lock) must not speak for the run.
  let admitted = false;
  let result;
  try {
    result = await executeRun(runDir, process.env, {
      onReady: async () => {
        admitted = true;
        if (workerHandshake) await sendWorkerReady(runDir);
      },
    });
  } catch (error) {
    process.exitCode = 1;
    // A terminal result may already be on disk (a later annotation failed):
    // then the run's own completion goes out, not an UNKNOWN notice.
    let saved;
    try {
      const current = readJson(join(runDir, "results.json"));
      if (isTerminal(current)) saved = current;
    } catch {
      // Unreadable results are exactly what the UNKNOWN notice reports.
    }
    process.stderr.write(!admitted
      ? `FAIL launcher: failed before admission: ${error.message}\n`
      : saved
        ? `FAIL launcher: runner error after its terminal result was saved: ${error.message}\n`
        : `FAIL launcher: no terminal result was saved: ${error.message}\n`);
    if (admitted) {
      try {
        if (!saved) {
          const outcome = await notifyRunnerFailure(runDir, error);
          if (outcome && (outcome.code !== 0 || outcome.error)) {
            process.stderr.write(`runner-failure notification failed: ${outcome.error ?? `exit ${outcome.code}`}\n`);
          }
        } else if (saved.notificationEligible !== false &&
            readJson(join(runDir, "request.json")).notifyTo) {
          await notifyRun(runDir);
        }
      } catch (notifyError) {
        process.stderr.write(`notification failed: ${notifyError.message}\n`);
      }
    }
    return;
  }
  process.stdout.write(await summarize(runDir));
  process.exitCode = result.exitCode;
  if (result.notificationEligible !== false && readJson(join(runDir, "request.json")).notifyTo) {
    try {
      await notifyRun(runDir);
    } catch (error) {
      process.stderr.write(`${error.message}\n`);
      process.exitCode ||= 1;
    }
  }
}

function closedBeforeReadyError(log, code, signal) {
  let detail = "";
  try {
    detail = readFileSync(log, "utf8").slice(-diagnosticLimit).trim();
  } catch {
    // The close status still identifies the failed admission.
  }
  return new Error(
    `detached worker closed before readiness (exit=${code ?? "unknown"}, signal=${signal ?? "none"})${detail ? `: ${detail}` : ""}`,
  );
}

export async function startDetachedRun(runDir, env = process.env) {
  const log = join(runDir, "launcher.log");
  const fd = openSync(log, "wx");
  try {
    const child = spawn(process.execPath, [script, "_run", runDir], {
      detached: true,
      windowsHide: true,
      stdio: ["ignore", fd, fd, "ipc"],
      env,
    });
    const completion = new Promise((resolveCompletion) => {
      child.once("exit", (code, signal) => resolveCompletion({ code, signal }));
    });
    await new Promise((resolveReady, rejectReady) => {
      let settled = false;
      const cleanup = () => {
        child.off("message", onMessage);
        child.off("error", onError);
        child.off("close", onClose);
      };
      const settle = (action, value) => {
        if (settled) return;
        settled = true;
        cleanup();
        action(value);
      };
      const onMessage = (message) => {
        if (message?.type === "READY" && message.runDir === resolve(runDir)) {
          settle(resolveReady);
        }
      };
      const onError = (error) => settle(rejectReady, error);
      const onClose = (code, signal) => {
        settle(rejectReady, closedBeforeReadyError(log, code, signal));
      };
      child.on("message", onMessage);
      child.once("error", onError);
      child.once("close", onClose);
    });
    if (child.connected) child.disconnect();
    child.unref();
    return { pid: child.pid, completion };
  } finally {
    closeSync(fd);
  }
}

function parseCommon(args) {
  let notifyTo;
  let detach = false;
  let liveBinary;
  let liveSha256;
  const requiredBinaryEnv = [];
  while (args.length && args[0] !== "--") {
    const flag = args.shift();
    if (flag === "--detach") detach = true;
    else if (flag === "--notify" && args.length) notifyTo = args.shift();
    else if (flag === "--require-binary-env" && /^[A-Z][A-Z0-9_]*$/u.test(args[0] ?? "")) {
      requiredBinaryEnv.push(args.shift());
    } else if (flag === "--engram-binary" && args.length) liveBinary = args.shift();
    else if (flag === "--engram-sha256" && args.length) liveSha256 = args.shift();
    else throw new Error(`invalid option: ${flag}`);
  }
  if (args[0] === "--") args.shift();
  return { notifyTo, detach, liveBinary, liveSha256, requiredBinaryEnv, rest: args };
}

async function main(args) {
  const mode = args.shift();
  if (["summary", "notify", "recover", "_run"].includes(mode)) {
    if (args.length !== 1) throw new Error(`${mode} requires one run directory`);
    const runDir = resolve(args[0]);
    if (mode === "summary") {
      process.stdout.write(await summarize(runDir));
      return;
    }
    if (mode === "notify") {
      await notifyRun(runDir);
      console.log(`Notification sent; tests not rerun. ${runDir}`);
      return;
    }
    if (mode === "recover") {
      const { settled, notification } = await recoverRun(runDir);
      process.stdout.write(await summarize(runDir));
      console.log(`${settled ? "Settled as interrupted" : "Already terminal"}; notification ${notification}; tests not rerun.`);
      return;
    }
    await finish(runDir, { workerHandshake: true });
    return;
  }
  if (!["full", "focused", "live"].includes(mode)) {
    throw new Error("usage: test-launcher.mjs full|focused|live [--notify SESSION] [--detach] [--require-binary-env NAME] [--engram-binary ABSOLUTE --engram-sha256 SHA256] [-- COMMAND ARGS...] | summary|notify|recover RUN_DIR");
  }
  const options = parseCommon(args);
  if (mode === "full" && options.rest.length) throw new Error("full takes no command");
  if (mode === "focused" && !options.rest.length) throw new Error("focused requires -- COMMAND ARGS");
  if (mode === "live" && (options.rest.length || !options.liveBinary || !options.liveSha256)) {
    throw new Error("live requires --engram-binary ABSOLUTE --engram-sha256 SHA256 and takes no command");
  }
  if (mode !== "live" && (options.liveBinary || options.liveSha256)) {
    throw new Error("Engram pin options are valid only in live mode");
  }
  if (options.detach && !options.notifyTo) {
    throw new Error("detached mode requires a different coordinator --notify SESSION; otherwise use foreground host completion wait");
  }
  const stages = mode === "full"
    ? requiredStages()
    : mode === "live"
      ? [liveStage()]
      : [{ name: "focused", command: options.rest[0], args: options.rest.slice(1) }];
  const runDir = await createRun({
    stages,
    notifyTo: options.notifyTo,
    requiredBinaryEnv: options.requiredBinaryEnv,
    prerequisiteFiles: mode === "full" ? requiredFiles() : mode === "live" ? [join(repository, "src", "tests", "engram_root_recovery_live.rs")] : [],
    needsCargo: mode === "full" || mode === "live",
    liveEngram: mode === "live"
      ? { path: options.liveBinary, expectedSha256: options.liveSha256 }
      : undefined,
    full: mode === "full",
    detached: Boolean(options.detach),
  });
  if (!options.detach) {
    // The run directory is known before any stage executes, so an interrupted
    // host wait can still inspect or settle this run without rerunning it.
    console.log(`RUN ${runDir}`);
    await finish(runDir);
    return;
  }
  // No inherited terminal handles: process lifetime is independent of the turn.
  // STARTED is printed only after the child owns admission and validates state.
  const detached = await startDetachedRun(runDir);
  console.log(`STARTED ${runDir}\npid=${detached.pid} completion=mailbox:${options.notifyTo.trim()}\nEnd your turn; do not poll. Missing terminal results mean running/interrupted, not PASS.`);
}

if (process.argv[1] && resolve(process.argv[1]) === script) {
  main(process.argv.slice(2)).catch((error) => {
    console.error(`FAIL launcher: ${error.message}`);
    process.exitCode = 1;
  });
}
