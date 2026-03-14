import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import css from "highlight.js/lib/languages/css";
import dart from "highlight.js/lib/languages/dart";
import diff from "highlight.js/lib/languages/diff";
import dockerfile from "highlight.js/lib/languages/dockerfile";
import go from "highlight.js/lib/languages/go";
import ini from "highlight.js/lib/languages/ini";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import markdown from "highlight.js/lib/languages/markdown";
import makefile from "highlight.js/lib/languages/makefile";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import shell from "highlight.js/lib/languages/shell";
import sql from "highlight.js/lib/languages/sql";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

hljs.registerLanguage("bash", bash);
hljs.registerLanguage("css", css);
hljs.registerLanguage("dart", dart);
hljs.registerLanguage("diff", diff);
hljs.registerLanguage("dockerfile", dockerfile);
hljs.registerLanguage("go", go);
hljs.registerLanguage("ini", ini);
hljs.registerLanguage("javascript", javascript);
hljs.registerLanguage("json", json);
hljs.registerLanguage("makefile", makefile);
hljs.registerLanguage("markdown", markdown);
hljs.registerLanguage("python", python);
hljs.registerLanguage("rust", rust);
hljs.registerLanguage("shell", shell);
hljs.registerLanguage("sql", sql);
hljs.registerLanguage("typescript", typescript);
hljs.registerLanguage("xml", xml);
hljs.registerLanguage("yaml", yaml);

const LANGUAGE_ALIASES: Record<string, string> = {
  bash: "bash",
  cjs: "javascript",
  css: "css",
  dart: "dart",
  diff: "diff",
  dockerfile: "dockerfile",
  go: "go",
  htm: "xml",
  html: "xml",
  ini: "ini",
  javascript: "javascript",
  js: "javascript",
  json: "json",
  jsx: "javascript",
  make: "makefile",
  makefile: "makefile",
  markdown: "markdown",
  md: "markdown",
  mdx: "markdown",
  mts: "typescript",
  postgres: "sql",
  psql: "sql",
  py: "python",
  python: "python",
  rs: "rust",
  rust: "rust",
  sh: "bash",
  shell: "shell",
  sql: "sql",
  svg: "xml",
  toml: "ini",
  ts: "typescript",
  tsx: "typescript",
  typescript: "typescript",
  xml: "xml",
  yaml: "yaml",
  yml: "yaml",
  zsh: "bash",
};

const FILE_PRINT_COMMAND_PATTERN =
  /\b(cat|sed|bat|head|tail|less|more|awk|perl|python|node|ruby)\b/i;
const PATH_HINT_PATTERN =
  /((?:\.{0,2}\/|~\/|\/)?[A-Za-z0-9_@%+~:-]+(?:\/[A-Za-z0-9_@%+~:.-]+)*\.[A-Za-z0-9_-]+)/g;

export function normalizeLanguage(language: string | null | undefined) {
  if (!language) {
    return null;
  }

  const cleaned = language.trim().toLowerCase().replace(/^language-/, "");
  const normalized = LANGUAGE_ALIASES[cleaned] ?? cleaned;

  return hljs.getLanguage(normalized) ? normalized : null;
}

export function inferLanguageFromPath(path: string | null | undefined) {
  if (!path) {
    return null;
  }

  const normalizedPath = path.trim().replace(/[?#].*$/, "");
  const lowerPath = normalizedPath.toLowerCase();
  const fileName = lowerPath.split("/").pop() ?? lowerPath;

  if (fileName === "dockerfile") {
    return "dockerfile";
  }

  if (fileName === "makefile") {
    return "makefile";
  }

  const extension = fileName.match(/\.([a-z0-9_-]+)$/)?.[1] ?? null;
  return normalizeLanguage(extension);
}

export function inferLanguageFromCommand(command: string) {
  const lowerCommand = command.toLowerCase();
  if (/\bgit\s+diff\b|\bdiff\s+-|\bdiff\s+--|\bpatch\b/.test(lowerCommand)) {
    return "diff";
  }

  if (!FILE_PRINT_COMMAND_PATTERN.test(command)) {
    return null;
  }

  const pathHints = Array.from(command.matchAll(PATH_HINT_PATTERN))
    .map((match) => match[1])
    .filter(Boolean);
  const pathHint = pathHints[pathHints.length - 1] ?? null;

  return inferLanguageFromPath(pathHint);
}

export function highlightCode(
  code: string,
  options?: {
    commandHint?: string | null;
    language?: string | null;
    pathHint?: string | null;
  },
) {
  const language =
    normalizeLanguage(options?.language) ??
    inferLanguageFromPath(options?.pathHint) ??
    (options?.commandHint ? inferLanguageFromCommand(options.commandHint) : null);

  if (!language) {
    return {
      language: null,
      html: escapeHtml(code),
    };
  }

  return {
    language,
    html: hljs.highlight(code, {
      ignoreIllegals: true,
      language,
    }).value,
  };
}

function escapeHtml(value: string) {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}
