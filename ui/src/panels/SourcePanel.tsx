import { Suspense, lazy, useEffect, useState } from "react";
import { copyTextToClipboard } from "../clipboard";
import type { MonacoCodeEditorStatus } from "../MonacoCodeEditor";
import { resolveMonacoLanguage, type MonacoAppearance } from "../monaco";

const MonacoCodeEditor = lazy(() =>
  import("../MonacoCodeEditor").then(({ MonacoCodeEditor }) => ({ default: MonacoCodeEditor })),
);

const DEFAULT_EDITOR_STATUS: MonacoCodeEditorStatus = {
  line: 1,
  column: 1,
  tabSize: 2,
  insertSpaces: true,
  endOfLine: "LF",
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

export type SourceFileState = {
  status: "idle" | "loading" | "ready" | "error";
  path: string;
  content: string;
  error: string | null;
  language: string | null;
};

export type SourcePanelFocus = {
  line: number;
  column: number | null;
  token: string | null;
};

export function SourcePanel({
  editorAppearance,
  editorFontSizePx,
  fileState,
  sourceFocus = null,
  sourcePath,
  onOpenInstructionDebugger,
  onSaveFile,
}: {
  editorAppearance: MonacoAppearance;
  editorFontSizePx: number;
  fileState: SourceFileState;
  sourceFocus?: SourcePanelFocus | null;
  sourcePath: string | null;
  onOpenInstructionDebugger?: (() => void) | null;
  onSaveFile: (path: string, content: string) => Promise<void>;
}) {
  const [editorValue, setEditorValue] = useState("");
  const [saveError, setSaveError] = useState<string | null>(null);
  const [isSaving, setIsSaving] = useState(false);
  const [editorStatus, setEditorStatus] = useState<MonacoCodeEditorStatus>(DEFAULT_EDITOR_STATUS);
  const [copiedPath, setCopiedPath] = useState(false);
  const isDirty = fileState.status === "ready" && editorValue !== fileState.content;

  useEffect(() => {
    if (fileState.status === "ready") {
      setEditorValue(fileState.content);
      setSaveError(null);
      setEditorStatus(createEditorStatusSnapshot(fileState.content));
      return;
    }

    if (fileState.status !== "loading") {
      setEditorValue("");
      setEditorStatus(DEFAULT_EDITOR_STATUS);
    }
  }, [fileState.content, fileState.path, fileState.status]);

  useEffect(() => {
    if (!copiedPath) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      setCopiedPath(false);
    }, 1600);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [copiedPath]);

  async function handleSave() {
    if (fileState.status !== "ready" || !isDirty || isSaving) {
      return;
    }

    setIsSaving(true);
    setSaveError(null);
    try {
      await onSaveFile(fileState.path, editorValue);
    } catch (error) {
      setSaveError(getErrorMessage(error));
    } finally {
      setIsSaving(false);
    }
  }

  async function handleCopyPath() {
    if (!displayPath) {
      return;
    }

    try {
      await copyTextToClipboard(displayPath);
      setCopiedPath(true);
    } catch {
      setCopiedPath(false);
    }
  }

  const saveStateLabel = saveError ? "Save failed" : isSaving ? "Saving..." : isDirty ? "Unsaved changes" : null;
  const displayPath = fileState.path.trim() || sourcePath?.trim() || "";
  const activeInstructionPath = displayPath;
  const canCopyPath = displayPath.length > 0;
  const canDebugInstructions =
    typeof onOpenInstructionDebugger === "function" &&
    isInstructionLikePath(activeInstructionPath);

  return (
    <div className={`source-pane${fileState.status === "ready" ? " has-editor" : ""}`}>
      <div className="source-toolbar">
        <div className="source-path-row source-path-display-row">
          <div
            className="source-path-display"
            role={fileState.status === "loading" ? "status" : undefined}
            aria-label={fileState.status === "loading" ? "Loading source file" : undefined}
            aria-live={fileState.status === "loading" ? "polite" : undefined}
            title={displayPath || undefined}
          >
            <span className="source-path-display-text">{displayPath || "No source file selected"}</span>
            {fileState.status === "loading" ? (
              <span className="activity-spinner source-path-loading-spinner" aria-hidden="true" />
            ) : null}
          </div>
          {(canCopyPath || canDebugInstructions) ? (
            <div className="source-path-actions">
              {canCopyPath ? (
                <button
                  className={`command-icon-button source-path-copy-button${copiedPath ? " copied" : ""}`}
                  type="button"
                  onClick={() => void handleCopyPath()}
                  aria-label={copiedPath ? "Path copied" : "Copy path"}
                  title={copiedPath ? "Copied" : "Copy path"}
                >
                  {copiedPath ? <CheckIcon /> : <CopyIcon />}
                </button>
              ) : null}
              {canDebugInstructions ? (
                <button
                  className="ghost-button source-toolbar-action"
                  type="button"
                  onClick={onOpenInstructionDebugger}
                >
                  Debug instructions
                </button>
              ) : null}
            </div>
          ) : null}
        </div>
      </div>

      {fileState.status === "idle" ? (
        <EmptyState
          title="No source file selected"
          body="Open a file from the workspace to show its source in this tile."
        />
      ) : null}

      {fileState.status === "error" ? (
        <article className="thread-notice">
          <div className="card-label">Source</div>
          <p>{fileState.error}</p>
        </article>
      ) : null}

      {saveError ? (
        <article className="thread-notice">
          <div className="card-label">Save failed</div>
          <p>{saveError}</p>
        </article>
      ) : null}

      {fileState.status === "ready" ? (
        <div className="source-editor-region">
          <div className="source-editor-shell source-editor-shell-with-statusbar">
            <Suspense fallback={<div className="source-editor-loading">Loading editor...</div>}>
              <MonacoCodeEditor
                appearance={editorAppearance}
                ariaLabel={`Source editor for ${fileState.path}`}
                fontSizePx={editorFontSizePx}
                highlightedColumnNumber={sourceFocus?.column ?? null}
                highlightedLineNumber={sourceFocus?.line ?? null}
                highlightToken={sourceFocus?.token ?? null}
                language={fileState.language}
                path={fileState.path}
                value={editorValue}
                onChange={setEditorValue}
                onSave={() => void handleSave()}
                onStatusChange={setEditorStatus}
              />
            </Suspense>
            <footer className="source-editor-statusbar" aria-label="Editor status">
              <div className="source-editor-statusbar-group">
                {saveStateLabel ? <span className="source-editor-statusbar-item source-editor-statusbar-state">{saveStateLabel}</span> : null}
              </div>
              <div className="source-editor-statusbar-group source-editor-statusbar-group-meta">
                <span className="source-editor-statusbar-item">{`Ln ${editorStatus.line}, Col ${editorStatus.column}`}</span>
                <span className="source-editor-statusbar-item">{formatIndentationLabel(editorStatus)}</span>
                <span className="source-editor-statusbar-item">UTF-8</span>
                <span className="source-editor-statusbar-item">{editorStatus.endOfLine}</span>
                <span className="source-editor-statusbar-item">{formatLanguageLabel(fileState.language, fileState.path)}</span>
              </div>
            </footer>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function EmptyState({ title, body }: { title: string; body: string }) {
  return (
    <article className="empty-state-card">
      <div className="card-label">Workspace</div>
      <h3>{title}</h3>
      <p>{body}</p>
    </article>
  );
}

function createEditorStatusSnapshot(content: string): MonacoCodeEditorStatus {
  return {
    ...DEFAULT_EDITOR_STATUS,
    endOfLine: content.includes("\r\n") ? "CRLF" : "LF",
  };
}

function formatIndentationLabel(status: MonacoCodeEditorStatus) {
  return status.insertSpaces ? `Spaces: ${status.tabSize}` : `Tab Size: ${status.tabSize}`;
}

function formatLanguageLabel(language: string | null, path: string) {
  const resolved = resolveMonacoLanguage(language, path);
  const normalizedPath = path.trim().toLowerCase();
  if (resolved === "typescript" && normalizedPath.endsWith(".tsx")) {
    return "TypeScript JSX";
  }
  if (resolved === "javascript" && normalizedPath.endsWith(".jsx")) {
    return "JavaScript JSX";
  }

  return LANGUAGE_LABELS[resolved] ?? resolved.replace(/[-_]+/g, " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function isInstructionLikePath(path: string) {
  const normalized = path.trim().toLowerCase().replace(/[\\/]+/g, "/");
  if (!normalized) {
    return false;
  }

  return (
    normalized.endsWith("/agents.md") ||
    normalized.endsWith("/claude.md") ||
    normalized.endsWith("/gemini.md") ||
    normalized.endsWith("/rules.md") ||
    normalized.endsWith("/skills.md") ||
    normalized.endsWith("/skill.md") ||
    normalized.endsWith("/agent.md") ||
    normalized.endsWith("/.claude.md") ||
    normalized.includes("/.claude/commands/") ||
    normalized.includes("/.claude/reviewers/") ||
    normalized.includes("/.cursor/rules/") ||
    normalized.includes("/skills/") ||
    normalized.includes("/rules/")
  );
}

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The request failed.";
}

function CopyIcon() {
  return (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <rect x="5" y="3" width="8" height="10" rx="1.5" />
      <path d="M3 11V5.5C3 4.67 3.67 4 4.5 4H10" />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M3.5 8.5 6.5 11.5 12.5 4.5" />
    </svg>
  );
}