import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import cssWorker from "monaco-editor/esm/vs/language/css/css.worker?worker";
import htmlWorker from "monaco-editor/esm/vs/language/html/html.worker?worker";
import jsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";
import tsWorker from "monaco-editor/esm/vs/language/typescript/ts.worker?worker";
import * as monaco from "monaco-editor/esm/vs/editor/editor.api";
import "monaco-editor/min/vs/editor/editor.main.css";

type MonacoEnvironment = {
  getWorker: (moduleId: string, label: string) => Worker;
};

let monacoConfigured = false;

export function ensureMonacoEnvironment() {
  if (!monacoConfigured) {
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
    monacoConfigured = true;
  }

  return monaco;
}

export type MonacoModule = typeof monaco;
export type MonacoAppearance = "light" | "dark";

export function monacoThemeName(appearance: MonacoAppearance) {
  return appearance === "dark" ? "vs-dark" : "vs";
}

export function resolveMonacoLanguage(language?: string | null, path?: string | null) {
  switch (language) {
    case "bash":
      return "shell";
    case "css":
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
