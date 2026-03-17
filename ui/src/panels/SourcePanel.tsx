import { Suspense, lazy, useEffect, useState, type KeyboardEvent } from "react";
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
  candidatePaths,
  editorAppearance,
  editorFontSizePx,
  fileState,
  sourceDraft,
  sourceFocus = null,
  sourcePath,
  onDraftChange,
  onOpenPath,
  onSaveFile,
}: {
  candidatePaths: string[];
  editorAppearance: MonacoAppearance;
  editorFontSizePx: number;
  fileState: SourceFileState;
  sourceDraft: string;
  sourceFocus?: SourcePanelFocus | null;
  sourcePath: string | null;
  onDraftChange: (nextValue: string) => void;
  onOpenPath: (path: string) => void;
  onSaveFile: (path: string, content: string) => Promise<void>;
}) {
  const [editorValue, setEditorValue] = useState("");
  const [saveError, setSaveError] = useState<string | null>(null);
  const [isSaving, setIsSaving] = useState(false);
  const [editorStatus, setEditorStatus] = useState<MonacoCodeEditorStatus>(DEFAULT_EDITOR_STATUS);
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

  function handlePathKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key !== "Enter") {
      return;
    }

    const nextPath = sourceDraft.trim();
    if (!nextPath) {
      return;
    }

    event.preventDefault();
    onOpenPath(nextPath);
  }

  const saveStateLabel = saveError ? "Save failed" : isSaving ? "Saving..." : isDirty ? "Unsaved changes" : null;

  return (
    <div className={`source-pane${fileState.status === "ready" ? " has-editor" : ""}`}>
      <div className="source-toolbar">
        <div className="source-path-row">
          <input
            className="source-path-input"
            type="text"
            value={sourceDraft}
            onChange={(event) => onDraftChange(event.target.value)}
            onKeyDown={handlePathKeyDown}
            placeholder="/absolute/path/to/file.rs"
          />
        </div>

        {candidatePaths.length > 0 ? (
          <div className="source-chip-row">
            {candidatePaths.map((path) => (
              <button
                key={path}
                className={`chip source-chip ${path === sourcePath ? "selected" : ""}`}
                type="button"
                onClick={() => onOpenPath(path)}
              >
                {path}
              </button>
            ))}
          </div>
        ) : null}
      </div>

      {fileState.status === "idle" ? (
        <EmptyState
          title="No source file selected"
          body="Pick a touched file above or enter a path manually to open the source in this tile."
        />
      ) : null}

      {fileState.status === "loading" ? (
        <article className="activity-card">
          <div className="activity-spinner" aria-hidden="true" />
          <div>
            <div className="card-label">Source</div>
            <h3>Loading file</h3>
            <p>{fileState.path}</p>
          </div>
        </article>
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

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The request failed.";
}
