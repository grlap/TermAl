import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import cssWorker from "monaco-editor/esm/vs/language/css/css.worker?worker";
import htmlWorker from "monaco-editor/esm/vs/language/html/html.worker?worker";
import jsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";
import tsWorker from "monaco-editor/esm/vs/language/typescript/ts.worker?worker";
import * as monaco from "monaco-editor/esm/vs/editor/editor.api";
import "monaco-editor/esm/vs/language/css/monaco.contribution";
import "monaco-editor/esm/vs/language/html/monaco.contribution";
import "monaco-editor/esm/vs/language/json/monaco.contribution";
import "monaco-editor/esm/vs/language/typescript/monaco.contribution";
import "monaco-editor/esm/vs/basic-languages/javascript/javascript.contribution";
import "monaco-editor/esm/vs/basic-languages/typescript/typescript.contribution";
import "monaco-editor/esm/vs/basic-languages/dart/dart.contribution";
import "monaco-editor/esm/vs/basic-languages/dockerfile/dockerfile.contribution";
import "monaco-editor/esm/vs/basic-languages/go/go.contribution";
import "monaco-editor/esm/vs/basic-languages/ini/ini.contribution";
import "monaco-editor/esm/vs/basic-languages/markdown/markdown.contribution";
import "monaco-editor/esm/vs/basic-languages/python/python.contribution";
import "monaco-editor/esm/vs/basic-languages/rust/rust.contribution";
import "monaco-editor/esm/vs/basic-languages/shell/shell.contribution";
import "monaco-editor/esm/vs/basic-languages/sql/sql.contribution";
import "monaco-editor/esm/vs/basic-languages/xml/xml.contribution";
import "monaco-editor/esm/vs/basic-languages/yaml/yaml.contribution";
import "monaco-editor/min/vs/editor/editor.main.css";
import { installMonacoCancellationRejectionFilter } from "./monaco-cancellation-filter";

type MonacoEnvironment = {
  getWorker: (moduleId: string, label: string) => Worker;
};

type MonacoThemeData = Parameters<typeof monaco.editor.defineTheme>[1];

type RGBA = {
  r: number;
  g: number;
  b: number;
  a: number;
};

let monacoConfigured = false;

export function ensureMonacoEnvironment() {
  installMonacoCancellationRejectionFilter();

  if (!monacoConfigured) {
    configureMonaco(monaco);
    monacoConfigured = true;
  }

  return monaco;
}

export type MonacoModule = typeof monaco;
export type MonacoAppearance = "light" | "dark";

export function monacoThemeName(appearance: MonacoAppearance) {
  return appearance === "dark" ? "termal-dark" : "termal-light";
}

export function applyMonacoTheme(monacoModule: MonacoModule, appearance: MonacoAppearance) {
  monacoModule.editor.defineTheme(monacoThemeName(appearance), buildMonacoTheme(appearance));
}

export function resolveMonacoLanguage(language?: string | null, path?: string | null) {
  switch (language) {
    case "bash":
      return "shell";
    case "css":
    case "dart":
    case "dockerfile":
    case "go":
    case "html":
    case "ini":
    case "javascript":
    case "json":
    case "markdown":
    case "python":
    case "rust":
    case "sql":
    case "typescript":
    case "xml":
    case "yaml":
      return language;
    default:
      return inferLanguageFromPath(path) ?? "plaintext";
  }
}

function configureMonaco(monacoModule: MonacoModule) {
  (globalThis as typeof globalThis & { MonacoEnvironment?: MonacoEnvironment }).MonacoEnvironment = {
    getWorker(_moduleId, label) {
      switch (label) {
        case "json":
          return new jsonWorker();
        case "css":
        case "less":
        case "scss":
          return new cssWorker();
        case "handlebars":
        case "html":
        case "razor":
        case "xml":
          return new htmlWorker();
        case "javascript":
        case "typescript":
          return new tsWorker();
        default:
          return new editorWorker();
      }
    },
  };
}

function buildMonacoTheme(appearance: MonacoAppearance): MonacoThemeData {
  const palette = readMonacoPalette(appearance);

  return {
    base: appearance === "dark" ? "vs-dark" : "vs",
    inherit: true,
    rules: [
      { token: "comment", foreground: toTokenHex(palette.muted) },
      { token: "keyword", foreground: toTokenHex(palette.signalRed) },
      { token: "keyword.control", foreground: toTokenHex(palette.signalRed) },
      { token: "operator", foreground: toTokenHex(palette.muted) },
      { token: "delimiter", foreground: toTokenHex(palette.muted) },
      { token: "string", foreground: toTokenHex(palette.signalGold) },
      { token: "regexp", foreground: toTokenHex(palette.signalGold) },
      { token: "number", foreground: toTokenHex(palette.signalBlue) },
      { token: "tag", foreground: toTokenHex(palette.signalRose) },
      { token: "attribute.name", foreground: toTokenHex(palette.signalBlue) },
      { token: "attribute.value", foreground: toTokenHex(palette.signalGold) },
      { token: "type", foreground: toTokenHex(palette.signalGreen) },
      { token: "type.identifier", foreground: toTokenHex(palette.signalGreen) },
      { token: "class", foreground: toTokenHex(palette.signalGreen) },
      { token: "function", foreground: toTokenHex(palette.signalBlue) },
      { token: "function.identifier", foreground: toTokenHex(palette.signalBlue) },
      { token: "variable.predefined", foreground: toTokenHex(palette.signalRose) },
      { token: "namespace", foreground: toTokenHex(palette.signalBlue) },
    ],
    colors: {
      "editor.background": toColorHex(palette.surface),
      "editor.foreground": toColorHex(palette.ink),
      "editorLineNumber.foreground": toColorHex(withAlpha(palette.muted, 0.78)),
      "editorLineNumber.activeForeground": toColorHex(palette.signalBlue),
      "editorCursor.foreground": toColorHex(palette.signalBlue),
      "editor.selectionBackground": toColorHex(palette.accentBlueBg),
      "editor.inactiveSelectionBackground": toColorHex(withAlpha(palette.accentBlueBg, 0.72)),
      "editor.selectionHighlightBackground": toColorHex(withAlpha(palette.signalBlue, 0.12)),
      "editor.wordHighlightBackground": toColorHex(withAlpha(palette.signalGold, 0.12)),
      "editor.wordHighlightStrongBackground": toColorHex(withAlpha(palette.signalGold, 0.18)),
      "editor.findMatchBackground": toColorHex(withAlpha(palette.signalGold, 0.22)),
      "editor.findMatchBorder": toColorHex(withAlpha(palette.signalGold, 0.52)),
      "editor.lineHighlightBackground": toColorHex(withAlpha(palette.signalBlue, 0.07)),
      "editor.lineHighlightBorder": "#00000000",
      "editorBracketHighlight.foreground1": toColorHex(withAlpha(palette.signalBlue, 0.78)),
      "editorBracketPairGuide.background1": toColorHex(withAlpha(palette.line, 0.78)),
      "editorBracketPairGuide.activeBackground1": toColorHex(withAlpha(palette.signalBlue, 0.45)),
      "editorIndentGuide.background1": toColorHex(withAlpha(palette.line, 0.78)),
      "editorIndentGuide.activeBackground1": toColorHex(withAlpha(palette.signalBlue, 0.45)),
      "editorGutter.background": toColorHex(palette.surface),
      "editorWhitespace.foreground": toColorHex(withAlpha(palette.muted, 0.22)),
      "editorOverviewRuler.border": "#00000000",
      "editorWidget.background": toColorHex(palette.panel),
      "editorWidget.foreground": toColorHex(palette.ink),
      "editorWidget.border": toColorHex(withAlpha(palette.line, 0.9)),
      "editorHoverWidget.background": toColorHex(palette.panel),
      "editorHoverWidget.border": toColorHex(withAlpha(palette.line, 0.9)),
      "editorSuggestWidget.background": toColorHex(palette.panel),
      "editorSuggestWidget.border": toColorHex(withAlpha(palette.line, 0.9)),
      "editorSuggestWidget.foreground": toColorHex(palette.ink),
      "editorSuggestWidget.selectedBackground": toColorHex(withAlpha(palette.signalBlue, 0.12)),
      "list.hoverBackground": toColorHex(withAlpha(palette.signalBlue, 0.08)),
      "list.activeSelectionBackground": toColorHex(withAlpha(palette.signalBlue, 0.14)),
      "list.activeSelectionForeground": toColorHex(palette.ink),
      "list.inactiveSelectionBackground": toColorHex(withAlpha(palette.signalBlue, 0.1)),
      "input.background": toColorHex(palette.surface),
      "input.foreground": toColorHex(palette.ink),
      "input.border": toColorHex(withAlpha(palette.line, 0.92)),
      "scrollbarSlider.background": toColorHex(withAlpha(palette.muted, 0.2)),
      "scrollbarSlider.hoverBackground": toColorHex(withAlpha(palette.muted, 0.32)),
      "scrollbarSlider.activeBackground": toColorHex(withAlpha(palette.signalBlue, 0.38)),
      "diffEditor.diagonalFill": toColorHex(withAlpha(palette.line, 0.48)),
      "diffEditor.insertedLineBackground": toColorHex(withAlpha(palette.signalGreen, appearance === "dark" ? 0.17 : 0.1)),
      "diffEditor.insertedTextBackground": toColorHex(withAlpha(palette.signalGreen, appearance === "dark" ? 0.3 : 0.18)),
      "diffEditor.removedLineBackground": toColorHex(withAlpha(palette.diffRemoved, appearance === "dark" ? 0.3 : 0.16)),
      "diffEditor.removedTextBackground": toColorHex(withAlpha(palette.diffRemoved, appearance === "dark" ? 0.46 : 0.28)),
      "diffEditorGutter.insertedLineBackground": toColorHex(withAlpha(palette.signalGreen, appearance === "dark" ? 0.55 : 0.36)),
      "diffEditorGutter.removedLineBackground": toColorHex(withAlpha(palette.diffRemoved, appearance === "dark" ? 0.72 : 0.42)),
    },
  };
}

function readMonacoPalette(appearance: MonacoAppearance) {
  return {
    ink: readCssColor("--ink", appearance === "dark" ? "#f4f2ec" : "#1d1718"),
    muted: readCssColor("--muted", appearance === "dark" ? "#9b9387" : "#766a6f"),
    line: readCssColor("--line", appearance === "dark" ? "rgba(198, 183, 159, 0.18)" : "rgba(38, 29, 33, 0.12)"),
    surface: readCssColor("--surface-white", appearance === "dark" ? "rgba(24, 26, 30, 0.92)" : "rgba(255, 255, 255, 0.88)"),
    panel: readCssColor("--panel-strong", appearance === "dark" ? "rgba(22, 23, 27, 0.96)" : "rgba(255, 255, 253, 0.96)"),
    signalBlue: readCssColor("--signal-blue", appearance === "dark" ? "#79d4ff" : "#667fbb"),
    signalGold: readCssColor("--signal-gold", appearance === "dark" ? "#f3cf7a" : "#c29a53"),
    signalGreen: readCssColor("--signal-green", appearance === "dark" ? "#59c97b" : "#368873"),
    signalRed: readCssColor("--signal-red", appearance === "dark" ? "#e07050" : "#cf6a52"),
    signalRose: readCssColor("--signal-rose", appearance === "dark" ? "#c18cff" : "#8f5a7a"),
    diffRemoved: readCssColor("--diff-removed-color", "#ef4444"),
    accentBlueBg: readCssColor("--accent-blue-bg", appearance === "dark" ? "rgba(121, 212, 255, 0.16)" : "rgba(102, 127, 187, 0.14)"),
  };
}

function readCssColor(variableName: string, fallback: string): RGBA {
  if (typeof window === "undefined" || typeof document === "undefined") {
    return parseCssColor(fallback) ?? { r: 0, g: 0, b: 0, a: 1 };
  }

  const rawValue = window.getComputedStyle(document.documentElement).getPropertyValue(variableName).trim();
  return parseCssColor(rawValue) ?? parseCssColor(fallback) ?? { r: 0, g: 0, b: 0, a: 1 };
}

function parseCssColor(value: string | null | undefined): RGBA | null {
  if (!value) {
    return null;
  }

  const trimmed = value.trim();
  if (!trimmed) {
    return null;
  }

  const hexMatch = trimmed.match(/^#([0-9a-f]{3,8})$/i);
  if (hexMatch) {
    return parseHexColor(hexMatch[1]);
  }

  const rgbMatch = trimmed.match(/^rgba?\((.+)\)$/i);
  if (!rgbMatch) {
    return null;
  }

  const normalized = rgbMatch[1].replace(/\s*\/\s*/g, ",").trim();
  const parts = normalized.includes(",")
    ? normalized.split(/\s*,\s*/)
    : normalized.split(/\s+/);

  if (parts.length < 3) {
    return null;
  }

  return {
    r: clampChannel(Number.parseFloat(parts[0] ?? "0")),
    g: clampChannel(Number.parseFloat(parts[1] ?? "0")),
    b: clampChannel(Number.parseFloat(parts[2] ?? "0")),
    a: clampAlpha(Number.parseFloat(parts[3] ?? "1")),
  };
}

function parseHexColor(hex: string): RGBA | null {
  const normalized = hex.trim();
  if (normalized.length === 3 || normalized.length === 4) {
    const [r, g, b, a = "f"] = normalized.split("");
    return parseHexColor(`${r}${r}${g}${g}${b}${b}${a}${a}`);
  }

  if (normalized.length !== 6 && normalized.length !== 8) {
    return null;
  }

  return {
    r: Number.parseInt(normalized.slice(0, 2), 16),
    g: Number.parseInt(normalized.slice(2, 4), 16),
    b: Number.parseInt(normalized.slice(4, 6), 16),
    a:
      normalized.length === 8
        ? clampAlpha(Number.parseInt(normalized.slice(6, 8), 16) / 255)
        : 1,
  };
}

function withAlpha(color: RGBA, alpha: number): RGBA {
  return {
    ...color,
    a: clampAlpha(color.a * alpha),
  };
}

function clampChannel(value: number) {
  if (!Number.isFinite(value)) {
    return 0;
  }

  return Math.max(0, Math.min(255, Math.round(value)));
}

function clampAlpha(value: number) {
  if (!Number.isFinite(value)) {
    return 1;
  }

  return Math.max(0, Math.min(1, value));
}

function toColorHex(color: RGBA) {
  const alpha = Math.round(color.a * 255);
  return alpha < 255
    ? `#${toHexPair(color.r)}${toHexPair(color.g)}${toHexPair(color.b)}${toHexPair(alpha)}`
    : `#${toHexPair(color.r)}${toHexPair(color.g)}${toHexPair(color.b)}`;
}

function toTokenHex(color: RGBA) {
  return `${toHexPair(color.r)}${toHexPair(color.g)}${toHexPair(color.b)}`;
}

function toHexPair(value: number) {
  return value.toString(16).padStart(2, "0");
}

function inferLanguageFromPath(path?: string | null) {
  const normalized = path?.trim().toLowerCase();
  if (!normalized) {
    return null;
  }

  if (normalized.endsWith("/dockerfile") || normalized.endsWith("\\dockerfile")) {
    return "dockerfile";
  }

  if (normalized.endsWith("/makefile") || normalized.endsWith("\\makefile")) {
    return "plaintext";
  }

  if (normalized.endsWith(".css")) {
    return "css";
  }
  if (normalized.endsWith(".dart")) {
    return "dart";
  }
  if (
    normalized.endsWith(".js") ||
    normalized.endsWith(".jsx") ||
    normalized.endsWith(".mjs") ||
    normalized.endsWith(".cjs")
  ) {
    return "javascript";
  }
  if (
    normalized.endsWith(".ts") ||
    normalized.endsWith(".tsx") ||
    normalized.endsWith(".mts")
  ) {
    return "typescript";
  }
  if (normalized.endsWith(".json")) {
    return "json";
  }
  if (normalized.endsWith(".md") || normalized.endsWith(".mdx")) {
    return "markdown";
  }
  if (normalized.endsWith(".py")) {
    return "python";
  }
  if (normalized.endsWith(".rs")) {
    return "rust";
  }
  if (normalized.endsWith(".sql")) {
    return "sql";
  }
  if (normalized.endsWith(".yaml") || normalized.endsWith(".yml")) {
    return "yaml";
  }
  if (
    normalized.endsWith(".html") ||
    normalized.endsWith(".htm") ||
    normalized.endsWith(".svg")
  ) {
    return "html";
  }

  return null;
}
