import { MarkdownContent, type MarkdownFileLinkTarget } from "./message-cards";
import type { MonacoAppearance } from "./monaco";

export type MarkdownDocumentCompleteness = "full" | "patch";

export function MarkdownDocumentView({
  appearance = "dark",
  completeness = "full",
  documentPath = null,
  markdown,
  note = null,
  onOpenSourceLink,
  title = "Rendered Markdown",
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  completeness?: MarkdownDocumentCompleteness;
  documentPath?: string | null;
  markdown: string;
  note?: string | null;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  title?: string;
  workspaceRoot?: string | null;
}) {
  const isEmpty = markdown.trim().length === 0;
  const completenessLabel = completeness === "full" ? "Full document" : "Patch preview";
  const visibleNote =
    note ??
    (completeness === "patch"
      ? "Rendered from patch context only. Unchanged document sections outside the diff are omitted."
      : null);

  return (
    <div className="markdown-document-view">
      <div className="markdown-document-header">
        <span className="chip">{title}</span>
        <span className="chip">{completenessLabel}</span>
      </div>
      {visibleNote ? <p className="support-copy markdown-document-note">{visibleNote}</p> : null}
      <div className="markdown-document-scroll">
        {isEmpty ? (
          <p className="support-copy markdown-document-empty">This Markdown document is empty.</p>
        ) : (
          <MarkdownContent
            appearance={appearance}
            documentPath={documentPath}
            markdown={markdown}
            onOpenSourceLink={onOpenSourceLink}
            showLineNumbers
            workspaceRoot={workspaceRoot}
          />
        )}
      </div>
    </div>
  );
}
