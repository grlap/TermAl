// A focused SDK process writes to its own run directory. Only the image
// captured at stage close can be used by registered Test verification.
import { createHash } from "node:crypto";
import { closeSync, constants, fstatSync, lstatSync, openSync, readSync, realpathSync } from "node:fs";
import { join } from "node:path";

const reportName = "focused.dart.json";
const reportLimit = 64 * 1024 * 1024;

function programName(command) {
  return command.split(/[\\/]/u).at(-1).toLowerCase();
}

function sdkRunner(command) {
  const name = programName(command);
  if (name === "flutter" || (process.platform === "win32" && name === "flutter.bat")) return "flutter";
  if (name === "dart" || (process.platform === "win32" && ["dart.exe", "dart.bat"].includes(name))) return "dart";
}

export function isDartSdkTest(command, args) {
  return args[0] === "test" && sdkRunner(command) !== undefined;
}

export function supportsFocusedDartTest(command, [verb, ...args]) {
  const runner = sdkRunner(command);
  if (!runner || verb !== "test") return false;
  const options = new Set();
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (!arg || /\p{Cc}/u.test(arg)) return false;
    if (!arg.startsWith("-")) continue;
    const option = arg === "-t" ? "--tags" : arg.startsWith("--concurrency=") ? "--concurrency" : arg;
    if (options.has(option)) return false;
    options.add(option);
    if (option === "--run-skipped" || (option === "--no-pub" && runner === "flutter")) continue;
    if (option === "--concurrency") {
      const count = arg.slice("--concurrency=".length);
      if (!/^\d+$/u.test(count) || Number(count) === 0 || Number(count) > 0xffffffff) return false;
      continue;
    }
    if (!["--exclude-tags", "--tags", "--plain-name"].includes(option)) return false;
    const value = args[++index];
    if (!value || /^\p{White_Space}*$/u.test(value) || value.startsWith("-") || /\p{Cc}/u.test(value)) return false;
  }
  return true;
}

function reportExists(path) {
  try {
    const metadata = lstatSync(path);
    if (!metadata.isFile() || realpathSync.native(path) !== path) {
      throw new Error("Dart JSON reporter must be a regular file at its original path");
    }
    return true;
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    return false;
  }
}

export function startDartReport(runDir, args) {
  const owned = join(realpathSync.native(runDir), reportName);
  if (reportExists(owned)) throw new Error("Dart JSON reporter already exists for this run");
  return { owned, args: [...args, "--file-reporter", `json:${owned}`] };
}

function readReport(owned) {
  if (!reportExists(owned)) return;
  const fd = openSync(owned, constants.O_RDONLY | (constants.O_NONBLOCK ?? 0) | (constants.O_NOFOLLOW ?? 0));
  try {
    const before = fstatSync(fd);
    if (!before.isFile() || before.size > reportLimit) return;
    const bytes = Buffer.alloc(before.size + 1);
    let length = 0;
    while (length < bytes.length) {
      const count = readSync(fd, bytes, length, bytes.length - length, null);
      if (!count) break;
      length += count;
    }
    const after = fstatSync(fd);
    if (length === before.size && after.size === length && after.mtimeMs === before.mtimeMs) {
      return bytes.subarray(0, length);
    }
  } finally {
    closeSync(fd);
  }
}

export function captureDartReportDigest(owned) {
  try {
    const bytes = readReport(owned);
    if (bytes) return `sha256:${createHash("sha256").update(bytes).digest("hex")}`;
  } catch {
    // An unreadable image is not evidence for this stage.
  }
}

export function focusedDartReportSummary(request, result, runDir) {
  if (request?.input !== "commit" || request.full || !Array.isArray(request.stages)) return;
  const stage = request.stages.find((entry) => entry?.name === "focused");
  if (typeof stage?.command !== "string" || !Array.isArray(stage.args) ||
      !stage.args.every((arg) => typeof arg === "string")) return;
  if (supportsFocusedDartTest(stage.command, stage.args)) {
    if (result.stages?.some((entry) => entry.name === "focused" && entry.dartReportDigest)) {
      try {
        const owned = join(realpathSync.native(runDir), reportName);
        if (reportExists(owned)) return `Dart JSON reporter: ${owned}`;
      } catch {
        // The captured report is no longer available at its original path.
      }
    }
    return "Dart JSON reporter: unavailable";
  }
  // Shell inspection here only chooses a diagnostic. Admission only uses
  // direct SDK commands, so arbitrary shell text can never grant credit.
  const shell = programName(stage.command).replace(/\.exe$/u, "");
  const wrapped = ["bash", "sh", "cmd", "powershell", "pwsh"].includes(shell) &&
    stage.args.some((arg) => /\b(?:flutter(?:\.bat)?|dart(?:\.bat|\.exe)?)["']?\s+test\b/iu.test(arg));
  if (isDartSdkTest(stage.command, stage.args) || wrapped) {
    return "Dart Test credit unavailable. Use node scripts/test-launcher.mjs focused --input commit -- flutter test "
      + "(or dart test) with supported filters and options and omit --file-reporter.";
  }
}
