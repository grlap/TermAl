import { existsSync, readFileSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, extname, join, relative, resolve, sep } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)));
const htmlPath = join(root, "index.html");
const cssPath = join(root, "styles.css");
const jsPath = join(root, "script.js");
const launchMode = process.argv.includes("--launch");
const failures = [];
const require = createRequire(import.meta.url);

const read = (path) => readFileSync(path, "utf8");
const fail = (message) => failures.push(message);
const html = read(htmlPath);
const css = read(cssPath);
const js = read(jsPath);

function matches(pattern, value = html) {
  return [...value.matchAll(pattern)];
}

function checkUniqueIds() {
  const ids = matches(/\sid=["']([^"']+)["']/gi).map((match) => match[1]);
  const duplicates = ids.filter((id, index) => ids.indexOf(id) !== index);
  if (duplicates.length) fail(`Duplicate HTML ids: ${[...new Set(duplicates)].join(", ")}`);
  return new Set(ids);
}

function checkReferences(ids) {
  const fragmentRefs = matches(/\shref=["']#([^"']+)["']/gi).map((match) => match[1]);
  const ariaRefs = matches(/\saria-(?:controls|describedby|labelledby|owns)=["']([^"']+)["']/gi)
    .flatMap((match) => match[1].trim().split(/\s+/));
  [...fragmentRefs, ...ariaRefs].forEach((id) => {
    if (!ids.has(id)) fail(`Reference points to missing id #${id}`);
  });
}

function checkLocalAssets() {
  const refs = matches(/\s(?:src|href)=["']([^"']+)["']/gi).map((match) => match[1]);
  refs.forEach((ref) => {
    if (/^(?:https?:|mailto:|tel:|#|data:)/i.test(ref)) return;
    const clean = decodeURIComponent(ref.split(/[?#]/)[0]);
    if (!clean) return;
    const target = resolve(root, clean);
    const insideRoot = target === root || target.startsWith(`${root}${sep}`);
    if (!insideRoot) fail(`Local asset escapes website/: ${ref}`);
    else if (!existsSync(target)) fail(`Missing local asset: ${ref}`);
  });
}

function checkExternalLinks() {
  matches(/<a\b([^>]*\bhref=["']https?:\/\/[^"']+["'][^>]*)>/gi).forEach((match) => {
    const attrs = match[1];
    if (/\btarget=["']_blank["']/i.test(attrs) && !/\brel=["'][^"']*\bnoopener\b[^"']*["']/i.test(attrs)) {
      fail("An external target=_blank link is missing rel=noopener");
    }
  });
}

function checkSemantics() {
  matches(/<button\b([^>]*)>/gi).forEach((match) => {
    if (!/\btype=["']button["']/i.test(match[1])) fail("Every button must explicitly use type=button");
  });
  if (/<(?:article|div|span)\b[^>]*\btabindex=["']0["']/i.test(html)) {
    fail("Non-interactive content must not be placed in the tab order");
  }
  if (/href=["']javascript:/i.test(html)) fail("javascript: links are not allowed");
  if (!/<html\b[^>]*class=["'][^"']*\bno-js\b/i.test(html)) fail("HTML must start with the no-js fallback class");
  if (!/<a\b[^>]*class=["'][^"']*skip-link/i.test(html)) fail("A keyboard skip link is required");
  if (!/<main\b[^>]*id=["']main["']/i.test(html)) fail("The skip-link target #main is required");
  if (!/aria-live=["']polite["']/i.test(html)) fail("The interactive workflow needs a polite live region");
}

function checkMetadata() {
  const jsonBlocks = matches(/<script\s+type=["']application\/ld\+json["']>([\s\S]*?)<\/script>/gi);
  if (jsonBlocks.length !== 1) fail(`Expected exactly one JSON-LD block, found ${jsonBlocks.length}`);
  else {
    try { JSON.parse(jsonBlocks[0][1]); } catch (error) { fail(`Invalid JSON-LD: ${error.message}`); }
  }

  const robots = html.match(/<meta\s+name=["']robots["']\s+content=["']([^"']+)["']/i)?.[1]?.toLowerCase() || "";
  if (launchMode) {
    if (!robots.includes("index") || robots.includes("noindex")) fail("Launch mode requires an indexable robots directive");
    if (!/<link\s+rel=["']canonical["']\s+href=["']https?:\/\//i.test(html)) fail("Launch mode requires an absolute canonical URL");
    if (!/<meta\s+property=["']og:url["']\s+content=["']https?:\/\//i.test(html)) fail("Launch mode requires og:url");
    if (!/<meta\s+property=["']og:image["']\s+content=["']https?:\/\//i.test(html)) fail("Launch mode requires an absolute og:image");
    if (!existsSync(join(root, "sitemap.xml"))) fail("Launch mode requires website/sitemap.xml");
    const repoRoot = resolve(root, "..");
    const license = readdirSync(repoRoot).some((name) => /^(?:license|copying)(?:\.|$)/i.test(name));
    if (!license) fail("Launch mode requires a repository-root LICENSE or COPYING file");
  } else if (!robots.includes("noindex") || !robots.includes("nofollow")) {
    fail("Prelaunch mode requires noindex,nofollow");
  }
}

function checkProgressiveEnhancement() {
  if (!/\.no-js\s+\.control-room__controls/.test(css)) fail("CSS must hide interactive-only controls in the no-JS fallback");
  if (!/\.no-js\s+\.site-header/.test(css)) fail("The fixed header needs a readable no-JS fallback");
  if (!/\.no-js\s+\.mobile-menu__panel/.test(css)) fail("The mobile menu needs a non-modal no-JS fallback");
  if (!/\.no-js\s+\*[\s\S]{0,180}animation:\s*none/.test(css)) fail("No-JS mode must not leave unpausable animations running");
  if (!/@media\s*\(prefers-reduced-motion:\s*reduce\)/.test(css)) fail("CSS must include a reduced-motion mode");
  if (!/\.motion-paused/.test(css)) fail("CSS must expose a global paused-motion state");
  if (!/\.motion-paused\s*\{[^}]*scroll-behavior:\s*auto/.test(css)) fail("Paused motion must disable smooth scrolling");
  if (!/classList\.replace\(["']no-js["'],\s*["']js["']\)/.test(js)) fail("JavaScript must enable enhancements only after initialization");
  if (!/__termalReady\s*=\s*true/.test(js)) fail("JavaScript must expose its browser-test readiness marker");
  if (!/dataset\.flowState/.test(js) || !/data-flow-step/.test(html)) fail("The control-room step interaction is not wired");
  const themeChoices = matches(/\sdata-theme-choice=["'][^"']+["']/gi);
  if (themeChoices.length !== 21) fail(`The theme catalog must expose all 21 themes, found ${themeChoices.length}`);
  const lightThemeChoices = matches(/<button\b[^>]*\bdata-theme-choice=["'][^"']+["'][^>]*\bdata-theme-kind=["']light["'][^>]*>/gi);
  const darkThemeChoices = matches(/<button\b[^>]*\bdata-theme-choice=["'][^"']+["'][^>]*\bdata-theme-kind=["']dark["'][^>]*>/gi);
  if (lightThemeChoices.length !== 10 || darkThemeChoices.length !== 11) {
    fail(`The paired theme catalog must expose 10 light and 11 dark themes, found ${lightThemeChoices.length} light and ${darkThemeChoices.length} dark`);
  }
  if (!/data-theme-choice/.test(js) || !/createThemeModeController/.test(js)) fail("The paired theme-system preview is not wired");
  if (!/Rendered Markdown/.test(html) || !/Save Markdown/.test(html) || !/INLINE VIEW ZONE/.test(html)) {
    fail("The Markdown diff and inline Mermaid differentiators must remain represented in the workspace");
  }
  if (!/workspace-demo--markdown[^>]*role=["']group["']/.test(html)) {
    fail("The text-rich Markdown workspace demo must remain exposed as an accessible group");
  }
  if (!/role=["']group["'][^>]*aria-label=["']10 light and 11 dark themes["']/.test(html)) {
    fail("The theme counts must expose their shared light/dark context");
  }
  if (!/Auto preview · Darkroom active/.test(html)) {
    fail("The static theme status must match the enhanced Auto preview wording");
  }
}

function checkThemeModeBehavior() {
  let createThemeModeController;
  try {
    ({ createThemeModeController } = require("./theme-demo-state.js"));
  } catch (error) {
    fail(`Theme demo state controller could not be loaded: ${error.message}`);
    return;
  }
  let systemChangeListener = null;
  const mediaQuery = {
    matches: false,
    addEventListener(type, listener) {
      if (type === "change") systemChangeListener = listener;
    },
    removeEventListener(type, listener) {
      if (type === "change" && systemChangeListener === listener) {
        systemChangeListener = null;
      }
    },
  };
  const observedKinds = [];
  const controller = createThemeModeController({
    initialMode: "auto",
    mediaQuery,
    onPreviewKindChange: (kind) => observedKinds.push(kind),
  });

  if (controller.getPreviewKind() !== "light") {
    fail("Auto mode must initialize from a light system preference");
  }
  mediaQuery.matches = true;
  systemChangeListener?.({ matches: true });
  if (observedKinds.at(-1) !== "dark") {
    fail("Auto mode must follow a live system change to dark");
  }
  controller.setMode("light");
  const observationsBeforeExplicitModeSystemChange = observedKinds.length;
  mediaQuery.matches = false;
  systemChangeListener?.({ matches: false });
  if (
    observedKinds.length !== observationsBeforeExplicitModeSystemChange ||
    controller.getPreviewKind() !== "light"
  ) {
    fail("An explicit theme mode must ignore later system scheme changes");
  }
  controller.setMode("auto");
  if (observedKinds.at(-1) !== "light") {
    fail("Returning to Auto must immediately adopt the current system scheme");
  }
  controller.dispose();
  if (systemChangeListener !== null) {
    fail("The theme mode controller must release its system scheme listener");
  }
}

function checkSourceTypes() {
  const allowed = new Set([".html", ".css", ".js", ".mjs", ".svg", ".md", ".xml", ".txt", ""]);
  const walk = (directory) => {
    readdirSync(directory, { withFileTypes: true }).forEach((entry) => {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (!allowed.has(extname(entry.name).toLowerCase())) fail(`Unexpected website file type: ${relative(root, path)}`);
    });
  };
  walk(root);
}

const ids = checkUniqueIds();
checkReferences(ids);
checkLocalAssets();
checkExternalLinks();
checkSemantics();
checkMetadata();
checkProgressiveEnhancement();
checkThemeModeBehavior();
checkSourceTypes();

if (failures.length) {
  console.error(`TermAl website verification failed (${failures.length}):`);
  failures.forEach((failure) => console.error(`- ${failure}`));
  process.exit(1);
}

console.log(`TermAl website verification passed (${launchMode ? "launch" : "prelaunch"} mode).`);
