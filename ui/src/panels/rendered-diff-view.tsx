// "Rendered" and "Markdown" diff-preview pieces used by the diff
// panel when the file under review is either a Markdown document or
// a non-Markdown file with renderable regions (Mermaid, math, Rust
// doc-comment markdown). Each piece either produces a preview
// payload the panel hands to its Markdown view, or renders a ready-
// to-display pane backed by `MarkdownContent`.
//
// What this file owns:
//   - `buildMarkdownDiffPreview` — pure builder that produces the
//     `MarkdownDiffPreviewModel` the panel passes to its Markdown
//     diff view. Prefers the backend-supplied `documentContent`
//     (full-document rendering: completeness = `"full"`) when
//     present; otherwise falls back to the structured patch
//     preview with completeness = `"patch"` and rolls the note
//     through from either the document content or the patch
//     enrichment note. Returns `null` when neither document nor
//     structured preview is available.
//   - `RenderedDiffView` — read-only renderer preview for non-
//     Markdown diff targets (Phase 4 of
//     `docs/features/source-renderers.md`). Composes each
//     detected renderable region into a synthetic Markdown
//     fragment (same strategy as
//     `./source-renderer-preview`'s `RendererPreviewPane`) and
//     routes through `MarkdownContent` so the existing safe
//     Mermaid / KaTeX wiring handles the actual rendering. Shows
//     a "Patch-only rendering" disclaimer when the backend didn't
//     supply a full document.
//   - `composeRenderedDiffMarkdown` — per-region synthetic-
//     Markdown assembler used by `RenderedDiffView`. Each region
//     gets a **Lines N–M** header followed by a fenced body:
//     ```` ```mermaid ```` for mermaid, `$$…$$` for math, the raw
//     body for markdown regions.
//
// What this file does NOT own:
//   - The Markdown diff view itself (`MarkdownDiffView`), the
//     segment stability logic, the Monaco wiring, or the edit
//     pipeline — those stay in `./DiffPanel.tsx`.
//   - The source-panel preview cluster
//     (`RendererPreviewPane` / `composeRendererPreviewMarkdown`
//     / `describeRenderableKinds`) — lives in
//     `./source-renderer-preview.tsx`. The two sibling helpers
//     (`composeRenderedDiffMarkdown` here vs.
//     `composeRendererPreviewMarkdown` there) are currently
//     near-identical: both produce the same `**Lines N-M**`
//     header followed by a fenced body and join regions with
//     `"\n\n"`. They were extracted as distinct consumers
//     because the source-panel and diff-panel preview chrome
//     differ (preview-pane vs. full diff shell with a Patch-only
//     disclaimer), and consolidating into a shared helper is a
//     future cleanup pass, not a pure code move.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same completeness
// strings ("full" / "patch"), same "Patch-only rendering"
// disclaimer copy, same **Lines N–M** header + fenced body
// format.

import { useMemo } from "react";
import type { GitDiffDocumentContent } from "../api";
import { MarkdownContent } from "../message-cards";
import type { MonacoAppearance } from "../monaco";
import type { SourceRenderableRegion } from "../source-renderers";
import type { DiffMessage } from "../types";
import type { buildDiffPreviewModel } from "../diff-preview";
import type {
  MarkdownDiffPreviewModel,
  MarkdownDocumentCompleteness,
} from "./markdown-diff-segments";

export function buildMarkdownDiffPreview(
  documentContent: GitDiffDocumentContent | null | undefined,
  documentEnrichmentNote: string | null | undefined,
  preview: ReturnType<typeof buildDiffPreviewModel>,
  changeType: DiffMessage["changeType"],
): MarkdownDiffPreviewModel | null {
  if (documentContent) {
    const completeness: MarkdownDocumentCompleteness = documentContent.isCompleteDocument ? "full" : "patch";
    return {
      before: {
        ...documentContent.before,
        completeness,
        note: documentContent.note ?? null,
      },
      after: {
        ...documentContent.after,
        completeness,
        note: documentContent.note ?? null,
      },
    };
  }

  if (!preview.hasStructuredPreview) {
    return null;
  }

  const completeness: MarkdownDocumentCompleteness = "patch";
  const note = documentEnrichmentNote ?? preview.note ?? null;
  return {
    before: {
      content: changeType === "create" ? "" : preview.originalText,
      source: "patch",
      completeness,
      note,
    },
    after: {
      content: preview.modifiedText,
      source: "patch",
      completeness,
      note,
    },
  };
}

// Read-only renderer preview for non-Markdown diff targets (Phase 4
// of `docs/features/source-renderers.md`). Composes each detected
// renderable region into a synthetic Markdown fragment (same
// strategy as `SourcePanel`'s `RendererPreviewPane`) and routes
// through `MarkdownContent` so the existing safe Mermaid/KaTeX
// wiring handles the actual rendering. Edits for the file's
// underlying source stay in Monaco's Edit mode — this view is
// intentionally display-only.
//
// Incomplete-document (patch-only) guard: when the backend returned
// a diff without `documentContent` (large files, unsupported binary
// types, read errors), the detected regions come from the
// after-side content we DO have — which might be the local worktree
// rather than the correct staged/unstaged side. We label the
// preview "Patch-only" so reviewers know the rendering is a best-
// effort approximation and can fall back to the raw diff for
// authoritative review.
export function RenderedDiffView({
  appearance,
  documentPath,
  isCompleteDocument,
  regions,
  workspaceRoot,
}: {
  appearance: MonacoAppearance;
  documentPath: string | null;
  isCompleteDocument: boolean;
  regions: SourceRenderableRegion[];
  workspaceRoot: string | null;
}) {
  const syntheticMarkdown = useMemo(
    () => composeRenderedDiffMarkdown(regions),
    [regions],
  );
  return (
    <div
      className="diff-rendered-view"
      aria-label="Rendered diff preview"
    >
      {!isCompleteDocument ? (
        <p className="support-copy diff-preview-note">
          Patch-only rendering: the backend did not supply the full
          document, so the preview is a best-effort approximation
          from the raw diff. Use the Raw patch view for authoritative
          review.
        </p>
      ) : null}
      <MarkdownContent
        appearance={appearance}
        documentPath={documentPath}
        markdown={syntheticMarkdown}
        workspaceRoot={workspaceRoot}
      />
    </div>
  );
}

export function composeRenderedDiffMarkdown(
  regions: SourceRenderableRegion[],
): string {
  if (regions.length === 0) {
    return "";
  }
  return regions
    .map((region) => {
      const header = `**Lines ${region.sourceStartLine}–${region.sourceEndLine}**`;
      if (region.renderer === "mermaid") {
        return `${header}\n\n\`\`\`mermaid\n${region.displayText.replace(/\s+$/, "")}\n\`\`\``;
      }
      if (region.renderer === "math") {
        return `${header}\n\n$$\n${region.displayText.replace(/\s+$/, "")}\n$$`;
      }
      return `${header}\n\n${region.displayText}`;
    })
    .join("\n\n");
}
