import { Suspense, lazy, useEffect, useMemo, useState } from "react";
import { fetchFile, type FileResponse } from "../api";
import { buildDiffPreviewModel } from "../diff-preview";
import type { MonacoAppearance } from "../monaco";
import type { DiffMessage } from "../types";
import { StructuredDiffView } from "./StructuredDiffView";

const MonacoCodeEditor = lazy(() =>
  import("../MonacoCodeEditor").then(({ MonacoCodeEditor }) => ({ default: MonacoCodeEditor })),
);

type DiffViewMode = "visual" | "latest" | "raw";

type LatestFileState = {
  status: "idle" | "loading" | "ready" | "error";
  path: string;
  content: string;
  error: string | null;
  language: string | null;
};

export function DiffPanel({
  appearance,
  changeType,
  diff,
  diffMessageId,
  filePath,
  language,
  onOpenPath,
  summary,
}: {
  appearance: MonacoAppearance;
  changeType: DiffMessage["changeType"];
  diff: string;
  diffMessageId: string;
  filePath: string | null;
  language?: string | null;
  onOpenPath: (path: string) => void;
  summary: string;
}) {
  const preview = useMemo(() => buildDiffPreviewModel(diff, changeType), [changeType, diff]);
  const [viewMode, setViewMode] = useState<DiffViewMode>(() =>
    defaultDiffViewMode(preview.hasStructuredPreview, Boolean(filePath)),
  );
  const [latestFile, setLatestFile] = useState<LatestFileState>(() => createInitialLatestFileState(filePath));

  useEffect(() => {
    setViewMode(defaultDiffViewMode(preview.hasStructuredPreview, Boolean(filePath)));
  }, [diffMessageId, filePath, preview.hasStructuredPreview]);

  useEffect(() => {
    let cancelled = false;

    if (!filePath) {
      setLatestFile(createInitialLatestFileState(null));
      return;
    }

    setLatestFile({
      status: "loading",
      path: filePath,
      content: "",
      error: null,
      language: language ?? null,
    });

    void fetchFile(filePath)
      .then((response) => {
        if (cancelled) {
          return;
        }

        setLatestFile(toLatestFileState(response));
      })
      .catch((error) => {
        if (cancelled) {
          return;
        }

        setLatestFile({
          status: "error",
          path: filePath,
          content: "",
          error: getErrorMessage(error),
          language: language ?? null,
        });
      });

    return () => {
      cancelled = true;
    };
  }, [filePath, language]);

  return (
    <div className="source-pane diff-preview-panel has-editor">
      <div className="source-toolbar">
        <div className="source-editor-toolbar">
          <div className="source-editor-status">
            <span className="chip">{changeType === "create" ? "New file" : "File edit"}</span>
            {preview.changeSummary.changedLineCount > 0 ? (
              <span className="chip diff-preview-stat diff-preview-stat-changed">
                Changed {preview.changeSummary.changedLineCount}
              </span>
            ) : null}
            {preview.changeSummary.addedLineCount > 0 ? (
              <span className="chip diff-preview-stat diff-preview-stat-added">
                Added {preview.changeSummary.addedLineCount}
              </span>
            ) : null}
            {preview.changeSummary.removedLineCount > 0 ? (
              <span className="chip diff-preview-stat diff-preview-stat-removed">
                Removed {preview.changeSummary.removedLineCount}
              </span>
            ) : null}
            {language ? <span className="chip">{language}</span> : null}
            {filePath ? <span className="chip">{filePath}</span> : null}
          </div>
          <div className="source-editor-actions diff-preview-actions">
            {preview.hasStructuredPreview ? (
              <button
                className={`ghost-button diff-preview-toggle ${viewMode === "visual" ? "selected" : ""}`}
                type="button"
                onClick={() => setViewMode("visual")}
              >
                Changes
              </button>
            ) : null}
            {filePath ? (
              <button
                className={`ghost-button diff-preview-toggle ${viewMode === "latest" ? "selected" : ""}`}
                type="button"
                onClick={() => setViewMode("latest")}
              >
                Latest file
              </button>
            ) : null}
            <button
              className={`ghost-button diff-preview-toggle ${viewMode === "raw" ? "selected" : ""}`}
              type="button"
              onClick={() => setViewMode("raw")}
            >
              Raw patch
            </button>
            {filePath ? (
              <button className="ghost-button" type="button" onClick={() => onOpenPath(filePath)}>
                Open file
              </button>
            ) : null}
          </div>
        </div>
        {summary ? <p className="support-copy file-viewer-summary diff-preview-summary">{summary}</p> : null}
      </div>

      <div className="source-editor-region diff-preview-region">
        {renderCurrentView({
          appearance,
          filePath,
          language,
          latestFile,
          preview,
          viewMode,
          diff,
        })}

        {viewMode !== "latest" && preview.note ? (
          <p className="support-copy diff-preview-note">{preview.note}</p>
        ) : null}
      </div>
    </div>
  );
}

function renderCurrentView({
  appearance,
  diff,
  filePath,
  language,
  latestFile,
  preview,
  viewMode,
}: {
  appearance: MonacoAppearance;
  diff: string;
  filePath: string | null;
  language?: string | null;
  latestFile: LatestFileState;
  preview: ReturnType<typeof buildDiffPreviewModel>;
  viewMode: DiffViewMode;
}) {
  if (viewMode === "latest") {
    if (!filePath) {
      return (
        <article className="thread-notice">
          <div className="card-label">Latest file</div>
          <p>This diff does not include a file path, so there is no latest file view to open.</p>
        </article>
      );
    }

    if (latestFile.status === "loading" || latestFile.status === "idle") {
      return <div className="source-editor-loading">Loading latest file...</div>;
    }

    if (latestFile.status === "error") {
      return (
        <article className="thread-notice">
          <div className="card-label">Latest file</div>
          <p>{latestFile.error}</p>
        </article>
      );
    }

    return (
      <div className="source-editor-shell">
        <Suspense fallback={<div className="source-editor-loading">Loading editor...</div>}>
          <MonacoCodeEditor
            appearance={appearance}
            ariaLabel={`Latest file view for ${latestFile.path}`}
            language={latestFile.language ?? language ?? null}
            path={latestFile.path}
            readOnly
            value={latestFile.content}
          />
        </Suspense>
      </div>
    );
  }

  if (viewMode === "visual" && preview.hasStructuredPreview) {
    return <StructuredDiffView filePath={filePath} preview={preview} />;
  }

  return <RawPatchView diff={diff} />;
}

function RawPatchView({ diff }: { diff: string }) {
  const lines = diff.split("\n");

  return (
    <div className="diff-editor-shell diff-preview-raw-shell">
      <div className="diff-preview-raw" role="table" aria-label="Raw patch preview">
        {lines.map((line, index) => (
          <div
            key={`${index}:${line}`}
            className={`diff-preview-raw-line ${rawDiffLineClassName(line)}`}
            role="row"
          >
            <span className="diff-preview-raw-line-number" aria-hidden="true">
              {index + 1}
            </span>
            <span className="diff-preview-raw-line-content" role="cell">
              {line || " "}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

function rawDiffLineClassName(line: string) {
  if (line.startsWith("@@")) {
    return "diff-preview-raw-line-hunk";
  }

  if (
    line.startsWith("diff --git ") ||
    line.startsWith("index ") ||
    line.startsWith("--- ") ||
    line.startsWith("+++ ")
  ) {
    return "diff-preview-raw-line-meta";
  }

  if (line === "\\ No newline at end of file") {
    return "diff-preview-raw-line-note";
  }

  if (line.startsWith("+") && !line.startsWith("+++ ")) {
    return "diff-preview-raw-line-added";
  }

  if (line.startsWith("-") && !line.startsWith("--- ")) {
    return "diff-preview-raw-line-removed";
  }

  return "diff-preview-raw-line-context";
}

function defaultDiffViewMode(hasStructuredPreview: boolean, hasFilePath: boolean): DiffViewMode {
  if (hasStructuredPreview) {
    return "visual";
  }

  return hasFilePath ? "latest" : "raw";
}

function createInitialLatestFileState(filePath: string | null): LatestFileState {
  if (!filePath) {
    return {
      status: "idle",
      path: "",
      content: "",
      error: null,
      language: null,
    };
  }

  return {
    status: "loading",
    path: filePath,
    content: "",
    error: null,
    language: null,
  };
}

function toLatestFileState(response: FileResponse): LatestFileState {
  return {
    status: "ready",
    path: response.path,
    content: response.content,
    error: null,
    language: response.language ?? null,
  };
}

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The request failed.";
}
