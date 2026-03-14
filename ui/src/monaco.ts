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

type MonacoEnvironment = {
  getWorker: (moduleId: string, label: string) => Worker;
};

let monacoConfigured = false;

export function ensureMonacoEnvironment() {
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

  monacoModule.editor.defineTheme("termal-light", {
    base: "vs",
    inherit: true,
    rules: [],
    colors: {
      "diffEditor.diagonalFill": "#efe8dd",
      "diffEditor.insertedLineBackground": "#e8f7eb",
      "diffEditor.insertedTextBackground": "#7cc89255",
      "diffEditor.removedLineBackground": "#fdecee",
      "diffEditor.removedTextBackground": "#f08d9b55",
      "diffEditorGutter.insertedLineBackground": "#78c48d",
      "diffEditorGutter.removedLineBackground": "#ef8a98",
      "editor.lineHighlightBorder": "#00000000",
      "editorIndentGuide.activeBackground1": "#9d8c72",
      "editorIndentGuide.background1": "#d8d2c7",
      "editorBracketPairGuide.activeBackground1": "#9d8c72",
      "editorBracketPairGuide.background1": "#d8d2c7",
      "editorBracketHighlight.foreground1": "#9d8c72",
    },
  });

  monacoModule.editor.defineTheme("termal-dark", {
    base: "vs-dark",
    inherit: true,
    rules: [],
    colors: {
      "diffEditor.diagonalFill": "#1d1d1d",
      "diffEditor.insertedLineBackground": "#173222",
      "diffEditor.insertedTextBackground": "#2f8f5b66",
      "diffEditor.removedLineBackground": "#412026",
      "diffEditor.removedTextBackground": "#b14a5c66",
      "diffEditorGutter.insertedLineBackground": "#2f8f5b",
      "diffEditorGutter.removedLineBackground": "#b14a5c",
      "editor.lineHighlightBorder": "#00000000",
      "editorIndentGuide.activeBackground1": "#8e8577",
      "editorIndentGuide.background1": "#3c3a38",
      "editorBracketPairGuide.activeBackground1": "#c6b79f",
      "editorBracketPairGuide.background1": "#45413c",
      "editorBracketHighlight.foreground1": "#c6b79f",
    },
  });
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
