// UI test categories for the shared test launcher.
//
// Owns: naming each UI category (`ui-<project>`) after the Vitest projects
// that ui/test-categories.ts declares, read from that manifest itself so there
// is no second list; resolving a category name to its project; listing the
// active UI test files; and accounting one Vitest run per project from that
// same run's JSON report: which selected files ran, which did not, test
// counts, and whether each project's files ran one at a time and the projects
// in their declared order.
// Does not own: which project a file belongs to (ui/test-categories.ts and its
// `projectSelects`), running stages, results files or the summary
// (test-launcher.mjs), or per-test durations (test-durations.mjs).
// New module.
import { readdirSync, readFileSync, realpathSync } from "node:fs";
import { isAbsolute, join, relative, sep } from "node:path";
import { pathToFileURL } from "node:url";

export const categoryPrefix = "ui-";
export const partialCheckLabel = "partial check, not the full gate";
// The project whose files run in the serialized lane: the architects' step-3
// decision names the existing heavy project, which the effective Vitest
// configuration runs one file at a time, in a group of its own.
export const serializedLaneProject = "heavy";
// Recorded file lists are bounded; their counts stay exact.
const recordedFileLimit = 50;

export async function loadCategoryManifest(root) {
  const manifest = await import(pathToFileURL(join(root, "ui", "test-categories.ts")).href);
  const projects = manifest.CATEGORY_PROJECTS;
  if (!Array.isArray(projects) || projects.length === 0 ||
      typeof manifest.projectSelects !== "function" ||
      projects.some((project) => typeof project?.name !== "string" ||
        !/^[a-z][a-z0-9-]*$/u.test(project.name) || !Number.isInteger(project.groupOrder))) {
    throw new Error("ui/test-categories.ts does not declare usable CATEGORY_PROJECTS and projectSelects");
  }
  if (!projects.some((project) => project.name === serializedLaneProject)) {
    throw new Error(`ui/test-categories.ts declares no ${serializedLaneProject} project for the serialized lane`);
  }
  return { projects, projectSelects: manifest.projectSelects };
}

export const categoryName = (project) => `${categoryPrefix}${project.name}`;

export function categoryNames(manifest) {
  return manifest.projects.map(categoryName);
}

// The project a category names; an unknown name is refused before any run.
export function resolveCategory(manifest, name) {
  const project = manifest.projects.find((candidate) => categoryName(candidate) === name);
  if (!project) {
    throw new Error(`unknown category: ${name}; known categories: ${categoryNames(manifest).join(", ")}`);
  }
  return project;
}

// Every UI test file under ui/src, as Vitest's project selections see it: a
// path relative to ui/ with forward slashes.
export function listUiTestFiles(uiRoot) {
  const files = [];
  const walk = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      if (entry.name === "node_modules") continue;
      const path = join(directory, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.isFile() && /\.test\.tsx?$/u.test(entry.name)) {
        files.push(relative(uiRoot, path).split(sep).join("/"));
      }
    }
  };
  walk(join(uiRoot, "src"));
  return files.sort();
}

// The UI root as given and, when the file system resolves it to another path,
// as resolved. A runner started in a directory names its files under the
// resolved path (macOS's /var is a link to /private/var), so a root reached
// through a link is compared in both forms, the given one first.
function uiRootBases(uiRoot) {
  const bases = [uiRoot];
  try {
    const resolved = realpathSync.native(uiRoot);
    if (resolved !== uiRoot) bases.push(resolved);
  } catch {
    // A root that cannot be resolved is compared as given.
  }
  return bases;
}

// The file's path relative to the first base that contains it, with forward
// slashes, or undefined when no base does.
function uiRelative(bases, file) {
  if (typeof file !== "string") return undefined;
  const native = file.split("/").join(sep);
  if (!isAbsolute(native)) return undefined;
  for (const base of bases) {
    const local = relative(base, native);
    if (local && !local.startsWith("..") && !isAbsolute(local)) return local.split(sep).join("/");
  }
  return undefined;
}

const bounded = (list) => list.slice(0, recordedFileLimit);

// Vitest's JSON reporter writes a file status of passed or failed, and maps
// each test's state to one of these assertion statuses: an intentional skip
// (skipped, todo) or an outcome it did not reach (pending).
const fileStatuses = new Set(["passed", "failed"]);
const assertionStatuses = new Set(["passed", "failed", "skipped", "todo", "pending"]);

// Why one file's result cannot be counted, or undefined when it is whole.
// Only a whole result may contribute to a pass.
function malformedFileResult(result) {
  if (!fileStatuses.has(result?.status)) return "no passed or failed file status";
  if (!Array.isArray(result.assertionResults)) return "no assertionResults list";
  if (result.assertionResults.some((assertion) => !assertionStatuses.has(assertion?.status))) {
    return "a test without a known status";
  }
  if (!Number.isFinite(result.startTime) || !Number.isFinite(result.endTime) ||
      result.endTime < result.startTime) {
    return "no valid start and end time";
  }
  return undefined;
}

// A file whose tests never ran (all skipped, or none) gets the reporter's run
// start as its time, so its window says nothing about order or overlap.
const executedTests = (result) => result.assertionResults
  .some((assertion) => assertion.status === "passed" || assertion.status === "failed");

// The files of one project overlap when one starts before another has ended:
// the project did not run them one at a time.
function overlappingFiles(files) {
  const timed = files
    .filter((file) => Number.isFinite(file.startTime) && Number.isFinite(file.endTime))
    .sort((left, right) => left.startTime - right.startTime);
  let overlaps = 0;
  let latestEnd = -Infinity;
  for (const file of timed) {
    if (file.startTime < latestEnd) overlaps += 1;
    latestEnd = Math.max(latestEnd, file.endTime);
  }
  return overlaps;
}

// Per-project child rows of one Vitest run, from that run's own JSON report;
// never from a second execution. `accounted` names the projects the run
// selected (all of them for the full gate). A row that cannot be established
// is unknown, never a pass, and the whole accounting is complete only when
// every row passed and nothing ran, or failed to run, outside the plan.
export function accountVitestRun({ manifest, uiRoot, baseline, accounted, reportText, reportError }) {
  const projects = manifest.projects.filter((project) => accounted.includes(project.name));
  const problems = [];
  const owners = new Map();
  for (const file of baseline) {
    const selecting = manifest.projects.filter((project) => manifest.projectSelects(project, file));
    if (selecting.length !== 1) {
      problems.push(`${file} is selected by ${selecting.length} projects, not exactly one`);
    } else {
      owners.set(file, selecting[0].name);
    }
  }
  const rowBase = (project) => ({
    kind: "test",
    category: categoryName(project),
    project: project.name,
    groupOrder: project.groupOrder,
    lane: project.name === serializedLaneProject ? "serialized" : "ordered",
    selectedFiles: baseline.filter((file) => owners.get(file) === project.name).length,
  });
  let report;
  if (reportError === undefined) {
    try {
      report = JSON.parse(reportText);
      if (!Array.isArray(report?.testResults)) throw new Error("the report has no testResults list");
    } catch (error) {
      reportError = `unreadable Vitest report: ${error.message}`;
    }
  }
  if (reportError !== undefined) {
    return {
      status: "unknown",
      reason: reportError,
      problems,
      projects: projects.map((project) => ({ ...rowBase(project), status: "unknown" })),
    };
  }
  const executed = new Map(projects.map((project) => [project.name, []]));
  const bases = uiRootBases(uiRoot);
  for (const result of report.testResults) {
    const file = uiRelative(bases, result?.name);
    const owner = file === undefined ? undefined : owners.get(file);
    if (owner === undefined || !executed.has(owner)) {
      problems.push(`${file ?? String(result?.name)} ran but no accounted project selects it`);
      continue;
    }
    executed.get(owner).push({ file, result });
  }
  const rows = projects.map((project) => {
    const row = rowBase(project);
    const ran = executed.get(project.name);
    const ranFiles = new Set(ran.map(({ file }) => file));
    const unrun = baseline.filter((file) => owners.get(file) === project.name && !ranFiles.has(file));
    const tests = { total: 0, passed: 0, failed: 0, skipped: 0, pending: 0 };
    let failedFiles = 0;
    const malformed = [];
    const times = [];
    for (const { file, result } of ran) {
      const why = malformedFileResult(result);
      if (why !== undefined) {
        malformed.push(`${file}: ${why}`);
        continue;
      }
      if (result.status === "failed") failedFiles += 1;
      for (const assertion of result.assertionResults) {
        tests.total += 1;
        if (assertion.status === "passed") tests.passed += 1;
        else if (assertion.status === "failed") tests.failed += 1;
        else if (assertion.status === "pending") tests.pending += 1;
        else tests.skipped += 1;
      }
      if (executedTests(result)) {
        times.push({ startTime: result.startTime, endTime: result.endTime });
      }
    }
    const starts = times.map((time) => time.startTime);
    const ends = times.map((time) => time.endTime);
    const overlaps = overlappingFiles(times);
    const status = ran.length === 0
      ? "empty"
      : failedFiles > 0 || tests.failed > 0
        ? "failed"
        : malformed.length > 0
          ? "unknown"
          : unrun.length > 0 || tests.pending > 0
            ? "incomplete"
            : overlaps > 0 ? "unserialized" : "passed";
    return {
      ...row,
      status,
      executedFiles: ran.length,
      unrunFiles: unrun.length,
      ...(unrun.length ? { unrun: bounded(unrun) } : {}),
      malformedFiles: malformed.length,
      ...(malformed.length ? { malformed: bounded(malformed) } : {}),
      failedFiles,
      tests,
      overlappingFiles: overlaps,
      ...(starts.length && ends.length
        ? { firstStart: Math.min(...starts), lastEnd: Math.max(...ends) }
        : {}),
    };
  });
  // Groups run one after another in their declared order: every project with a
  // test window starts no earlier than the latest end among all strictly
  // earlier groups. Projects sharing a group may overlap each other. A project
  // with no window (all its tests skipped) adds no bound and removes none;
  // that is timing only, never a pass on its own.
  const groupOrders = [...new Set(rows.map((row) => row.groupOrder))].sort((left, right) => left - right);
  let latest;
  for (const groupOrder of groupOrders) {
    const windowed = rows.filter((row) => row.groupOrder === groupOrder &&
      Number.isFinite(row.firstStart) && Number.isFinite(row.lastEnd));
    for (const row of windowed) {
      if (latest && row.firstStart < latest.lastEnd) {
        problems.push(`${row.category} started before ${latest.category} ended`);
      }
    }
    for (const row of windowed) {
      if (!latest || row.lastEnd > latest.lastEnd) latest = row;
    }
  }
  const complete = problems.length === 0 && rows.every((row) => row.status === "passed");
  return {
    status: complete ? "complete" : "incomplete",
    ...(complete ? {} : { reason: [
      ...rows.filter((row) => row.status !== "passed").map((row) => `${row.category} ${row.status}`),
      ...problems,
    ].slice(0, recordedFileLimit).join("; ") }),
    problems: bounded(problems),
    projects: rows,
  };
}

// Reads the report a stage wrote and accounts it; never throws for a missing
// or unreadable report, which leaves every row unknown.
export function accountVitestArtifact({ manifest, uiRoot, accounted, artifact }) {
  let reportText;
  let reportError;
  try {
    reportText = readFileSync(artifact, "utf8");
  } catch (error) {
    reportError = error.code === "ENOENT" ? "the runner wrote no Vitest report" : error.message;
  }
  return accountVitestRun({
    manifest,
    uiRoot,
    baseline: listUiTestFiles(uiRoot),
    accounted,
    reportText,
    reportError,
  });
}

// One summary line per project row, after the run's scope line.
export function accountingSummaryLines(stages) {
  const lines = [];
  for (const stage of stages) {
    const accounting = stage.accounting;
    if (!accounting) continue;
    lines.push(`${stage.name} projects: ${accounting.status}${accounting.reason ? ` (${accounting.reason.slice(0, 400)})` : ""}`);
    for (const row of accounting.projects ?? []) {
      const lane = row.lane === "serialized" ? " [serialized lane]" : "";
      const detail = !row.tests
        ? `${row.selectedFiles} files selected, outcome unknown`
        : `${row.executedFiles}/${row.selectedFiles} files, ${row.tests.passed} passed, ${row.tests.failed} failed, ${row.tests.skipped} skipped${row.tests.pending ? `, ${row.tests.pending} not finished` : ""}${row.malformedFiles ? `, ${row.malformedFiles} unreadable file results` : ""}${row.unrunFiles ? `, ${row.unrunFiles} unrun` : ""}${row.overlappingFiles ? `, ${row.overlappingFiles} overlapping` : ""}`;
      lines.push(`  ${row.category}${lane}: ${row.status}: ${detail}`);
    }
  }
  return lines;
}
