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
//     `docs/features/source-renderers.md`). Renders each detected
//     renderable region in its own `<section data-rendered-diff
//     -region-index="N">` wrapper with a `**Lines N–M**` header,
//     and routes the per-region body through `MarkdownContent` so
//     the existing safe Mermaid / KaTeX wiring handles the actual
//     rendering. The per-region split exposes the regions as
//     scroll targets for prev/next change navigation. Shows a
//     "Patch-only rendering" disclaimer when the backend didn't
//     supply a full document. Footer carries prev/next change
//     navigation buttons + a "Region X of Y" counter, mirroring
//     the Monaco diff-editor and the rendered-Markdown diff view.
//   - `composeRenderedDiffRegionMarkdown` — per-region synthetic-
//     Markdown assembler. The `**Lines N–M**` header is now
//     emitted by the wrapper element (so navigation can scroll to
//     it via DOM); this helper returns just the fenced body:
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
//     `./source-renderer-preview.tsx`.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same completeness
// strings ("full" / "patch"), same "Patch-only rendering"
// disclaimer copy, same **Lines N–M** header + fenced body
// format.

import { useCallback, useEffect, useRef, useState } from "react";
import type { GitDiffDocumentContent } from "../api";
import { MarkdownContent } from "../message-cards";
import type { MonacoAppearance } from "../monaco";
import type { SourceRenderableRegion } from "../source-renderers";
import type { DiffMessage } from "../types";
import type { buildDiffPreviewModel } from "../diff-preview";
import { DiffNavArrow } from "./DiffPanelIcons";
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
  const regionCount = regions.length;
  const [currentRegionIndex, setCurrentRegionIndex] = useState(0);
  // `navigationTick` advances on every prev/next press so the scroll
  // effect below fires even when `currentRegionIndex` does not change.
  // Without this, the single-region case (`regionCount === 1`) wraps
  // the index from 0 to 0; React bails on the no-op state set, the
  // effect never re-runs, and prev/next appear dead. Mirrors the
  // `MarkdownDiffView` navigation contract.
  const [navigationTick, setNavigationTick] = useState(0);
  // Clamp the index when the region set shrinks (e.g., the user
  // edited the upstream source so a region is gone). Falls back to 0
  // when there are no regions; the counter UI handles the empty case
  // via `regionCount === 0`.
  useEffect(() => {
    setCurrentRegionIndex((current) => {
      if (regionCount === 0) {
        return 0;
      }
      if (current >= regionCount) {
        return regionCount - 1;
      }
      return current;
    });
  }, [regionCount]);
  const goToPreviousRegion = useCallback(() => {
    setCurrentRegionIndex((current) =>
      current <= 0 ? Math.max(regionCount - 1, 0) : current - 1,
    );
    setNavigationTick((tick) => tick + 1);
  }, [regionCount]);
  const goToNextRegion = useCallback(() => {
    setCurrentRegionIndex((current) =>
      current >= regionCount - 1 ? 0 : current + 1,
    );
    setNavigationTick((tick) => tick + 1);
  }, [regionCount]);
  // Scroll the active region into view when the index advances via
  // prev/next. Initial mount intentionally skips the scroll so the
  // parent's restored scroll position survives — the user has not
  // pressed a navigation button yet. Mirrors the
  // `MarkdownDiffView` scroll-into-view contract.
  const scrollContainerRef = useRef<HTMLDivElement | null>(null);
  const lastScrolledIndexRef = useRef<number | null>(null);
  const lastScrolledNavigationTickRef = useRef(0);
  useEffect(() => {
    if (regionCount === 0) {
      lastScrolledIndexRef.current = null;
      return;
    }
    if (lastScrolledIndexRef.current === null) {
      // Initial mount: record the baseline but skip the scroll so the
      // parent's restored scroll position survives.
      lastScrolledIndexRef.current = currentRegionIndex;
      lastScrolledNavigationTickRef.current = navigationTick;
      return;
    }
    const indexChanged =
      lastScrolledIndexRef.current !== currentRegionIndex;
    const navigationRequested =
      lastScrolledNavigationTickRef.current !== navigationTick;
    if (!indexChanged && !navigationRequested) {
      return;
    }
    const container = scrollContainerRef.current;
    if (!container) {
      return;
    }
    const target = container.querySelector<HTMLElement>(
      `[data-rendered-diff-region-index="${currentRegionIndex}"]`,
    );
    if (!target) {
      return;
    }
    target.scrollIntoView({ block: "center" });
    lastScrolledIndexRef.current = currentRegionIndex;
    lastScrolledNavigationTickRef.current = navigationTick;
  }, [currentRegionIndex, navigationTick, regionCount]);

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
      <div className="diff-rendered-view-scroll" ref={scrollContainerRef}>
        {regions.map((region, index) => (
          <section
            className="diff-rendered-region"
            data-rendered-diff-region-index={index}
            key={region.id}
          >
            <div className="diff-rendered-region-header">
              <strong>{`Lines ${region.sourceStartLine}–${region.sourceEndLine}`}</strong>
            </div>
            <MarkdownContent
              appearance={appearance}
              documentPath={documentPath}
              markdown={composeRenderedDiffRegionMarkdown(region)}
              workspaceRoot={workspaceRoot}
            />
          </section>
        ))}
      </div>
      <footer
        className="source-editor-statusbar diff-preview-statusbar"
        aria-label="Rendered diff status"
      >
        <div className="source-editor-statusbar-group">
          <div className="diff-preview-change-nav" aria-label="Change navigation">
            <button
              className="diff-preview-nav-button"
              type="button"
              onClick={goToPreviousRegion}
              disabled={regionCount === 0}
              aria-label="Previous region"
              title="Previous region"
            >
              <DiffNavArrow direction="up" />
            </button>
            <button
              className="diff-preview-nav-button"
              type="button"
              onClick={goToNextRegion}
              disabled={regionCount === 0}
              aria-label="Next region"
              title="Next region"
            >
              <DiffNavArrow direction="down" />
            </button>
          </div>
          <span
            className="source-editor-statusbar-item source-editor-statusbar-state"
            aria-live="polite"
            aria-atomic="true"
          >
            {regionCount === 0
              ? "No rendered regions"
              : `Region ${currentRegionIndex + 1} of ${regionCount}`}
          </span>
        </div>
        <div className="source-editor-statusbar-group source-editor-statusbar-group-meta">
          <span className="source-editor-statusbar-item">
            {isCompleteDocument ? "Full document" : "Patch preview"}
          </span>
        </div>
      </footer>
    </div>
  );
}

/**
 * Per-region synthetic Markdown body. The `**Lines N–M**` header is
 * NOT included here — `RenderedDiffView` emits the header in JSX so
 * the section wrapper can carry the data-attribute used by the
 * change-navigation scroll handler.
 */
export function composeRenderedDiffRegionMarkdown(
  region: SourceRenderableRegion,
): string {
  if (region.renderer === "mermaid") {
    return `\`\`\`mermaid\n${region.displayText.replace(/\s+$/, "")}\n\`\`\``;
  }
  if (region.renderer === "math") {
    return `$$\n${region.displayText.replace(/\s+$/, "")}\n$$`;
  }
  return region.displayText;
}
