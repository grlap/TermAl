// Guards the UI test category manifest (ui/test-categories.ts): every test
// file under ui/ is in exactly one Vitest project, every *.test.ts is placed
// on purpose, every file that renders the App is in the app project, and the
// projects run in their declared order.
import { beforeAll, describe, expect, it } from "vitest";
import {
  APP_TEST_FILES,
  CATEGORY_PROJECTS,
  COMPONENT_TSX_GLOB,
  DOM_TS_TEST_FILES,
  HEAVY_TEST_FILES,
  UNIT_TEST_FILES,
  projectSelects,
  type ResourceTag,
} from "../test-categories";

// The UI sources carry no Node types, so the file system is loaded the way
// the other file-reading tests here load it.
type DirectoryEntry = { name: string; isDirectory(): boolean };
type FileSystem = {
  existsSync(path: string): boolean;
  readFileSync(path: string, encoding: "utf8"): string;
  readdirSync(path: string, options: { withFileTypes: true }): DirectoryEntry[];
};

let fs: FileSystem;
// Vitest runs with ui/ as its root and working directory.
const uiRoot = (globalThis as typeof globalThis & { process: { cwd(): string } }).process
  .cwd()
  .split("\\")
  .join("/");
let testFiles: string[] = [];

// Directories Vitest never collects from; everything else under ui/ is
// scanned, so a test file outside src/ cannot silently fall out of every project.
const skippedDirectories = new Set(["node_modules", "dist", "coverage"]);

function testFilesUnder(directory: string): string[] {
  const found: string[] = [];
  const absolute = directory ? `${uiRoot}/${directory}` : uiRoot;
  for (const entry of fs.readdirSync(absolute, { withFileTypes: true })) {
    const path = directory ? `${directory}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      if (!skippedDirectories.has(entry.name) && !entry.name.startsWith(".")) {
        found.push(...testFilesUnder(path));
      }
    } else if (/\.(test|spec)\.[cm]?[jt]sx?$/u.test(entry.name)) {
      found.push(path);
    }
  }
  return found.sort();
}

const source = (file: string) => fs.readFileSync(`${uiRoot}/${file}`, "utf8");
const heavyFiles = HEAVY_TEST_FILES.map(({ file }) => file);

// The harness exports that render the App; a test that calls one renders it.
const APP_RENDERING_HARNESS_EXPORTS = [
  "renderApp",
  "renderAppWithProjectAndSession",
  "withFallbackStateHarness",
];
const HARNESS = "src/app-test-harness.tsx";
const importsAppModule =
  /(?:from\s+|import\(\s*)["'](?:\.\.?\/)+App(?:\.tsx|\.ts|\.js)?["']/u;
const callsAppRenderingHelper = new RegExp(
  `\\b(?:${APP_RENDERING_HARNESS_EXPORTS.join("|")})\\s*\\(`,
  "u",
);
const rendersAppDirectly = (text: string) =>
  importsAppModule.test(text) || callsAppRenderingHelper.test(text);

function resolveLocalModule(fromFile: string, specifier: string): string | undefined {
  const parts = fromFile.split("/").slice(0, -1);
  for (const segment of specifier.split("/")) {
    if (segment === "..") parts.pop();
    else if (segment !== ".") parts.push(segment);
  }
  const base = parts.join("/");
  return [base, `${base}.ts`, `${base}.tsx`, `${base}.js`].find(
    (candidate) => /\.[jt]sx?$/u.test(candidate) && fs.existsSync(`${uiRoot}/${candidate}`),
  );
}

// A test renders the App when it does so itself, or through a local
// non-test module it imports (one level, e.g. a fixture module). The harness
// itself is covered by the helper-call rule above: importing it for other
// helpers does not render the App.
//
// Limits, stated so nobody reads more into a pass: this is a text match, not
// an import graph. A helper call is recognised by name only, and modules are
// followed one level deep, so a file that renders the App through a deeper
// chain of local modules, or through a renamed alias, is not detected.
function rendersApp(file: string): boolean {
  const text = source(file);
  if (rendersAppDirectly(text)) return true;
  for (const [, specifier] of text.matchAll(/from\s+["'](\.\.?\/[^"']+)["']/gu)) {
    const module = resolveLocalModule(file, specifier);
    if (!module || module === HARNESS || /\.test\.[jt]sx?$/u.test(module)) continue;
    if (rendersAppDirectly(source(module))) return true;
  }
  return false;
}

beforeAll(async () => {
  const nodeFsModule = "node:fs";
  fs = (await import(nodeFsModule)) as FileSystem;
  testFiles = testFilesUnder("");
});

describe("UI test categories", () => {
  it("runs from ui/ and finds the suite's test files, all under src/ and named *.test.ts or *.test.tsx", () => {
    expect(fs.existsSync(`${uiRoot}/test-categories.ts`)).toBe(true);
    expect(testFiles.length).toBeGreaterThan(200);
    expect(testFiles.filter((file) => !/^src\/.+\.test\.tsx?$/u.test(file))).toEqual([]);
  });

  it("puts every test file in exactly one project, so none runs twice or not at all", () => {
    const misplaced = testFiles
      .map((file) => ({
        file,
        projects: CATEGORY_PROJECTS.filter((project) => projectSelects(project, file)).map(
          ({ name }) => name,
        ),
      }))
      .filter(({ projects }) => projects.length !== 1);
    expect(misplaced).toEqual([]);
  });

  it("selects files only by exact paths and the one component glob, which the guard's matcher understands", () => {
    const patterns = CATEGORY_PROJECTS.flatMap(({ include, exclude }) => [...include, ...exclude]);
    expect(
      patterns.filter((pattern) => pattern !== COMPONENT_TSX_GLOB && /[*?[\]{}!]/u.test(pattern)),
    ).toEqual([]);
  });

  // A source check, not an evaluation of the config: it confirms the
  // projects are built from the manifest, and the gate's per-file results
  // confirm which files each project actually ran.
  it("builds the Vitest projects from the manifest", () => {
    expect(source("vite.config.ts")).toMatch(/projects:\s*CATEGORY_PROJECTS\.map\(/u);
  });

  it("lists only existing files, each once across every list", () => {
    const listed = [...APP_TEST_FILES, ...heavyFiles, ...UNIT_TEST_FILES, ...DOM_TS_TEST_FILES];
    expect(listed.filter((file) => !fs.existsSync(`${uiRoot}/${file}`))).toEqual([]);
    expect(listed.filter((file, index) => listed.indexOf(file) !== index)).toEqual([]);
  });

  it("places every *.test.ts on purpose, as unit, as needing the DOM, or as rendering the App", () => {
    const placed = new Set([...UNIT_TEST_FILES, ...DOM_TS_TEST_FILES, ...APP_TEST_FILES]);
    expect(testFiles.filter((file) => file.endsWith(".test.ts") && !placed.has(file))).toEqual([]);
    expect(
      [...UNIT_TEST_FILES, ...DOM_TS_TEST_FILES].filter((file) => !file.endsWith(".test.ts")),
    ).toEqual([]);
  });

  // A static backstop for the unit contract: a unit file needs no DOM. It
  // rejects the listed patterns in each unit file's own text, which names the
  // cause before the file's own run does, and a per-file environment comment,
  // which would give that one file jsdom inside the unit project. It does not
  // read the modules a test imports. For those, the unit project's node
  // environment fails a file whose run reaches a DOM global node lacks, such
  // as document or window. Neither check catches browser storage reached
  // through an import, since test-setup supplies in-memory storage in every
  // project, nor code behind a typeof check that takes its non-DOM branch.
  it("keeps every unit file free of Testing Library, rendering and the DOM globals", () => {
    const needsTheDom: Record<string, RegExp> = {
      "a Testing Library import": /from\s+["']@testing-library\//u,
      renderHook: /\brenderHook\b/u,
      "render(": /(?<![\w.])render\s*\(/u,
      "screen.": /\bscreen\./u,
      "document.": /\bdocument\./u,
      "window.": /\bwindow\./u,
      "browser storage": /\b(?:localStorage|sessionStorage)\b/u,
      "a per-file environment comment": /@vitest-environment\b/u,
    };
    // This guard is a unit file too. It reads files only through node:fs, but
    // its own patterns spell the words they look for, so it is not scanned.
    const self = "src/test-categories.test.ts";
    const offenders = UNIT_TEST_FILES.filter((file) => file !== self).flatMap((file) => {
      const text = source(file);
      const uses = Object.entries(needsTheDom)
        .filter(([, pattern]) => pattern.test(text))
        .map(([name]) => name);
      return uses.length ? [`${file}: ${uses.join(", ")}`] : [];
    });
    expect(offenders).toEqual([]);
  });

  it("knows every harness export that renders the App", () => {
    const harness = source(HARNESS);
    const rendering = harness
      .split(/\nexport /u)
      .slice(1)
      .flatMap((part) => {
        const name = /^(?:async\s+)?function\s+(\w+)/u.exec(part)?.[1];
        return name && (/<App\b/u.test(part) || callsAppRenderingHelper.test(part)) ? [name] : [];
      });
    expect(rendering.sort()).toEqual([...APP_RENDERING_HARNESS_EXPORTS].sort());
  });

  it("puts a file that renders the App in the app project, and only such files", () => {
    const app = new Set(APP_TEST_FILES);
    expect(testFiles.filter((file) => rendersApp(file) && !app.has(file))).toEqual([]);
    expect(APP_TEST_FILES.filter((file) => !rendersApp(file))).toEqual([]);
  });

  it("tags heavy files only with known resource tags and gives each a reason", () => {
    const known: readonly ResourceTag[] = ["cpu-heavy", "wall-clock"];
    for (const { file, tags, reason } of HEAVY_TEST_FILES) {
      expect(file.endsWith(".test.tsx"), file).toBe(true);
      expect(tags.filter((tag) => !known.includes(tag)), file).toEqual([]);
      expect(new Set(tags).size, file).toBe(tags.length);
      expect(reason.trim(), file).not.toBe("");
    }
  });

  // The unit project has no DOM, so a unit file whose run reaches a DOM
  // global node lacks, even through a module it imports, fails. Every other
  // project keeps jsdom.
  it("runs the unit project in node and every other project in jsdom", () => {
    expect(
      Object.fromEntries(CATEGORY_PROJECTS.map(({ name, environment }) => [name, environment])),
    ).toEqual({ unit: "node", component: "jsdom", heavy: "jsdom", app: "jsdom" });
    expect(source("vite.config.ts")).toMatch(/environment,/u);
    // This guard is itself a unit file, so its own run shows the environment
    // Vitest applied after merging the root and project configs, which the
    // source check above cannot.
    expect(typeof document).toBe("undefined");
    expect(typeof window).toBe("undefined");
  });

  it("runs unit, component, heavy and app in that order, each in its own group", () => {
    expect(CATEGORY_PROJECTS.map(({ name }) => name)).toEqual([
      "unit",
      "component",
      "heavy",
      "app",
    ]);
    const orders = CATEGORY_PROJECTS.map(({ groupOrder }) => groupOrder);
    expect(orders).toEqual([...orders].sort((left, right) => left - right));
    expect(new Set(orders).size).toBe(orders.length);
    // Vitest 4 runs a one-worker, isolated project with groupOrder 0 after
    // every ordered group, whatever its place in this list.
    expect(orders.filter((order) => order < 1)).toEqual([]);
  });
});
