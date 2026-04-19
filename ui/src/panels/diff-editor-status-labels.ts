// Default-status constants and pure label / snapshot helpers for
// the Monaco editor + diff editor status bar used by the diff panel.
// None of this knows about React state — the diff panel owns the
// `useState` wrappers; this module only produces typed snapshots
// and strings.
//
// What this file owns:
//   - `DEFAULT_EDITOR_STATUS` — the baseline `MonacoCodeEditorStatus`
//     (line 1 / column 1 / 2-space indent / LF) used as the initial
//     state of the inline edit view and as a reset target when the
//     panel clears its editor state.
//   - `DEFAULT_DIFF_EDITOR_STATUS` — the baseline
//     `MonacoDiffEditorStatus` extending the code-editor default
//     with `changeCount: 0`, `currentChange: 0`.
//   - `createEditorStatusSnapshot` — builds a fresh
//     `MonacoCodeEditorStatus` for a content string, sniffing
//     CRLF vs LF from the content so the status bar shows the
//     right line-ending on first paint.
//   - Label formatters:
//     `formatChangeNavigationLabel` (e.g. `Change 3 of 12`,
//     `No changes`), `formatIndentationLabel` (e.g. `Spaces: 2`
//     / `Tab Size: 4`), `formatLanguageLabel` (pretty language
//     name with `.tsx` / `.jsx` suffix awareness),
//     `isMarkdownDocument` (resolves Monaco language to decide
//     whether markdown-specific UI applies).
//   - `LANGUAGE_LABELS` — the pretty-name lookup map consumed by
//     `formatLanguageLabel`. Private to the module.
//
// What this file does NOT own:
//   - Monaco language resolution (`resolveMonacoLanguage`) — lives
//     in `../monaco` and is imported here.
//   - The `MonacoCodeEditorStatus` / `MonacoDiffEditorStatus`
//     types — live with their respective editor wrappers in
//     `../MonacoCodeEditor` and `../MonacoDiffEditor`.
//   - The editor state / handle refs, the status-bar JSX, or the
//     Monaco mount wiring — all of that stays in `./DiffPanel.tsx`
//     alongside the React components.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same defaults, same
// label copy, same TSX / JSX suffix specialisation.

import type { MonacoCodeEditorStatus } from "../MonacoCodeEditor";
import type { MonacoDiffEditorStatus } from "../MonacoDiffEditor";
import { resolveMonacoLanguage } from "../monaco";

export const DEFAULT_EDITOR_STATUS: MonacoCodeEditorStatus = {
  line: 1,
  column: 1,
  tabSize: 2,
  insertSpaces: true,
  endOfLine: "LF",
};

export const DEFAULT_DIFF_EDITOR_STATUS: MonacoDiffEditorStatus = {
  ...DEFAULT_EDITOR_STATUS,
  changeCount: 0,
  currentChange: 0,
};

const LANGUAGE_LABELS: Record<string, string> = {
  bash: "Shell Script",
  css: "CSS",
  dockerfile: "Dockerfile",
  go: "Go",
  html: "HTML",
  ini: "INI",
  javascript: "JavaScript",
  json: "JSON",
  markdown: "Markdown",
  plaintext: "Plain Text",
  powershell: "PowerShell",
  python: "Python",
  rust: "Rust",
  shell: "Shell Script",
  sql: "SQL",
  typescript: "TypeScript",
  xml: "XML",
  yaml: "YAML",
};

export function createEditorStatusSnapshot(content: string): MonacoCodeEditorStatus {
  return {
    ...DEFAULT_EDITOR_STATUS,
    endOfLine: content.includes("\r\n") ? "CRLF" : "LF",
  };
}

export function formatChangeNavigationLabel(status: MonacoDiffEditorStatus) {
  if (status.changeCount === 0) {
    return "No changes";
  }

  return `Change ${Math.max(status.currentChange, 1)} of ${status.changeCount}`;
}

export function formatIndentationLabel(status: MonacoCodeEditorStatus) {
  return status.insertSpaces ? `Spaces: ${status.tabSize}` : `Tab Size: ${status.tabSize}`;
}

export function formatLanguageLabel(language: string | null | undefined, path: string | null | undefined) {
  const resolved = resolveMonacoLanguage(language ?? null, path ?? null);
  const normalizedPath = path?.trim().toLowerCase() ?? "";
  if (resolved === "typescript" && normalizedPath.endsWith(".tsx")) {
    return "TypeScript JSX";
  }
  if (resolved === "javascript" && normalizedPath.endsWith(".jsx")) {
    return "JavaScript JSX";
  }

  return LANGUAGE_LABELS[resolved] ?? resolved.replace(/[-_]+/g, " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
}

export function isMarkdownDocument(language: string | null | undefined, path: string | null | undefined) {
  return resolveMonacoLanguage(language ?? null, path ?? null) === "markdown";
}
