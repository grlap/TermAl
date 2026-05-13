// Read-only Markdown document chrome used by renderer-preview surfaces.
//
// What this file owns:
//   - `MarkdownDocumentView` — a small wrapper that renders an
//     optional document note, the empty-document state, and the scroll
//     container around `MarkdownContent`.
//
// What this file does NOT own:
//   - Markdown parsing/rendering, Mermaid iframe sizing, link targets,
//     or line-number rendering. Those remain in `message-cards.tsx`.
//   - Editable rendered-Markdown commit plumbing. SourcePanel's
//     editable preview uses `EditableRenderedMarkdownSection` instead.

import { MarkdownContent, type MarkdownFileLinkTarget } from "./message-cards";
import type { MonacoAppearance } from "./monaco";

export function MarkdownDocumentView({
  appearance = "dark",
  documentPath = null,
  fillMermaidAvailableSpace = false,
  markdown,
  note = null,
  onOpenSourceLink,
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  documentPath?: string | null;
  fillMermaidAvailableSpace?: boolean;
  markdown: string;
  note?: string | null;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  workspaceRoot?: string | null;
}) {
  const isEmpty = markdown.trim().length === 0;
  const visibleNote = note;

  return (
    <div className="markdown-document-view">
      {visibleNote ? <p className="support-copy markdown-document-note">{visibleNote}</p> : null}
      <div className="markdown-document-scroll">
        {isEmpty ? (
          <p className="support-copy markdown-document-empty">This Markdown document is empty.</p>
        ) : (
          <MarkdownContent
            appearance={appearance}
            documentPath={documentPath}
            fillMermaidAvailableSpace={fillMermaidAvailableSpace}
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
