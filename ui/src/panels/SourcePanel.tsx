import { Suspense, lazy, useEffect, useState } from "react";
import type { MonacoAppearance } from "../monaco";

const MonacoCodeEditor = lazy(() =>
  import("../MonacoCodeEditor").then(({ MonacoCodeEditor }) => ({ default: MonacoCodeEditor })),
);

export type SourceFileState = {
  status: "idle" | "loading" | "ready" | "error";
  path: string;
  content: string;
  error: string | null;
  language: string | null;
};

export function SourcePanel({
  candidatePaths,
  editorAppearance,
  fileState,
  sourceDraft,
  sourcePath,
  onDraftChange,
  onOpenPath,
  onSaveFile,
}: {
  candidatePaths: string[];
  editorAppearance: MonacoAppearance;
  fileState: SourceFileState;
  sourceDraft: string;
  sourcePath: string | null;
  onDraftChange: (nextValue: string) => void;
  onOpenPath: (path: string) => void;
  onSaveFile: (path: string, content: string) => Promise<void>;
}) {
  const [editorValue, setEditorValue] = useState("");
  const [saveError, setSaveError] = useState<string | null>(null);
  const [isSaving, setIsSaving] = useState(false);
  const isDirty = fileState.status === "ready" && editorValue !== fileState.content;

  useEffect(() => {
    if (fileState.status === "ready") {
      setEditorValue(fileState.content);
      setSaveError(null);
      return;
    }

    if (fileState.status !== "loading") {
      setEditorValue("");
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

  return (
    <div className="source-pane">
      <div className="source-toolbar">
        <div className="source-path-row">
          <input
            className="source-path-input"
            type="text"
            value={sourceDraft}
            onChange={(event) => onDraftChange(event.target.value)}
            placeholder="/absolute/path/to/file.rs"
          />
          <button
            className="ghost-button"
            type="button"
            onClick={() => onOpenPath(sourceDraft.trim())}
            disabled={!sourceDraft.trim()}
          >
            Open
          </button>
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

        {fileState.status === "ready" ? (
          <div className="source-editor-toolbar">
            <div className="source-editor-status">
              <span className="chip">{isDirty ? "Unsaved changes" : "Saved"}</span>
              <span className="chip">{fileState.language ?? "plain text"}</span>
            </div>
            <div className="source-editor-actions">
              <button
                className="ghost-button"
                type="button"
                onClick={() => {
                  setEditorValue(fileState.content);
                  setSaveError(null);
                }}
                disabled={!isDirty || isSaving}
              >
                Revert
              </button>
              <button
                className="new-session-button"
                type="button"
                onClick={() => void handleSave()}
                disabled={!isDirty || isSaving}
              >
                {isSaving ? "Saving..." : "Save"}
              </button>
            </div>
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
        <article className="message-card source-file-card">
          <div className="message-meta">
            <span>Source</span>
            <span>{fileState.path}</span>
          </div>
          <div className="source-editor-shell">
            <Suspense fallback={<div className="source-editor-loading">Loading editor...</div>}>
              <MonacoCodeEditor
                appearance={editorAppearance}
                ariaLabel={`Source editor for ${fileState.path}`}
                language={fileState.language}
                path={fileState.path}
                value={editorValue}
                onChange={setEditorValue}
                onSave={() => void handleSave()}
              />
            </Suspense>
          </div>
          <p className="support-copy source-editor-hint">
            Save with the button above or use {primaryModifierLabel()}+S.
          </p>
        </article>
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

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The request failed.";
}

function primaryModifierLabel() {
  if (typeof navigator === "undefined") {
    return "Ctrl";
  }

  return navigator.platform.toLowerCase().includes("mac") ? "Cmd" : "Ctrl";
}
