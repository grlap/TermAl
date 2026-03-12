import { Suspense, lazy, useEffect, useMemo, useState } from "react";
import { buildDiffPreviewModel } from "../diff-preview";
import type { MonacoAppearance } from "../monaco";
import type { DiffMessage } from "../types";

const MonacoDiffEditor = lazy(() =>
  import("../MonacoDiffEditor").then(({ MonacoDiffEditor }) => ({ default: MonacoDiffEditor })),
);

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
  const [viewMode, setViewMode] = useState<"visual" | "raw">(
    preview.hasStructuredPreview ? "visual" : "raw",
  );

  useEffect(() => {
    setViewMode(preview.hasStructuredPreview ? "visual" : "raw");
  }, [diffMessageId, preview.hasStructuredPreview]);

  return (
    <div className="source-pane diff-preview-panel">
      <div className="source-toolbar">
        <div className="source-editor-toolbar">
          <div className="source-editor-status">
            <span className="chip">{changeType === "create" ? "New file" : "File edit"}</span>
            {language ? <span className="chip">{language}</span> : null}
            {filePath ? <span className="chip">{filePath}</span> : null}
          </div>
          <div className="source-editor-actions diff-preview-actions">
            {preview.hasStructuredPreview ? (
              <>
                <button
                  className={`ghost-button diff-preview-toggle ${viewMode === "visual" ? "selected" : ""}`}
                  type="button"
                  onClick={() => setViewMode("visual")}
                >
                  Visual
                </button>
                <button
                  className={`ghost-button diff-preview-toggle ${viewMode === "raw" ? "selected" : ""}`}
                  type="button"
                  onClick={() => setViewMode("raw")}
                >
                  Raw patch
                </button>
              </>
            ) : null}
            {filePath ? (
              <button className="ghost-button" type="button" onClick={() => onOpenPath(filePath)}>
                Open file
              </button>
            ) : null}
          </div>
        </div>
      </div>

      <article className="message-card diff-preview-card">
        <div className="message-meta">
          <span>Diff</span>
          <span>{filePath ?? "Patch preview"}</span>
        </div>
        <p className="support-copy">{summary}</p>
        {viewMode === "visual" && preview.hasStructuredPreview ? (
          <div className="diff-editor-shell">
            <Suspense fallback={<div className="source-editor-loading">Loading diff editor...</div>}>
              <MonacoDiffEditor
                appearance={appearance}
                ariaLabel={filePath ? `Diff preview for ${filePath}` : "Diff preview"}
                language={language}
                path={filePath}
                modifiedValue={preview.modifiedText}
                originalValue={preview.originalText}
              />
            </Suspense>
          </div>
        ) : (
          <pre className="code-block diff-block diff-preview-raw">{diff}</pre>
        )}
        {preview.note ? <p className="support-copy diff-preview-note">{preview.note}</p> : null}
      </article>
    </div>
  );
}
