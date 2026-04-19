// Rendered-preview pane shown alongside the Monaco editor in the
// source panel. Phase 3 of `docs/features/source-renderers.md`:
// when a non-Markdown file has at least one recognised renderable
// region (Mermaid, math, Rust doc-comment markdown), the preview
// pane composes those regions into a synthetic Markdown fragment
// that `MarkdownContent` already knows how to render safely, so the
// sandboxed-Mermaid and KaTeX paths from Phase 1 are reused without
// a second renderer implementation. Markdown source files delegate
// straight to `MarkdownDocumentView` so the existing Markdown
// chrome (headings / ToC / link handling) stays intact.
//
// What this file owns:
//   - `RendererPreviewPane` — the React component rendered in the
//     preview column of the source panel's split view. Picks
//     `<MarkdownDocumentView>` for Markdown source files, else
//     composes a synthetic-Markdown fragment and hands it to
//     `<MarkdownContent>`.
//   - `composeInlineRegionFence` — wraps a single renderable region
//     in the minimal Markdown code-fence / math-block that
//     `MarkdownContent` needs: ```` ```mermaid ```` for mermaid,
//     `$$…$$` for math, the raw display text for markdown.
//     Exported because the source panel also portals individual
//     inline regions through `<MarkdownContent>` via the Monaco
//     view-zone machinery.
//   - `composeRendererPreviewMarkdown` — joins every region into a
//     single Markdown document by prefacing each with a
//     **Lines N–M** header (so the user can cross-reference
//     Monaco) and separating entries with a blank line.
//   - `composeRendererPreviewRegion` — per-region body helper for
//     `composeRendererPreviewMarkdown`. Mirrors
//     `composeInlineRegionFence` but always promotes math to the
//     block `$$…$$` form because inline math would be awkward in a
//     preview column.
//   - `describeRenderableKinds` — builds the short kind label for
//     the preview-pane header (e.g. `Mermaid`, `Mermaid + Math`,
//     or `Document` when the regions list is empty). De-duplicates
//     on renderer name and sorts the result alphabetically.
//
// What this file does NOT own:
//   - Region detection (`detectRenderableRegions`, mermaid / math
//     fence-language guards) — lives in `../source-renderers`.
//   - `<MarkdownContent>` / `<MarkdownDocumentView>` themselves and
//     the link-target type — live in `../message-cards` and
//     `../MarkdownDocumentView`.
//   - Panel chrome, Monaco editor wiring, view-zone portals —
//     those stay with `<SourcePanel>`.
//
// Split out of `ui/src/panels/SourcePanel.tsx`. Same markup, same
// fence syntax, same header copy, same kind-label ordering.

import { MarkdownDocumentView } from "../MarkdownDocumentView";
import { MarkdownContent, type MarkdownFileLinkTarget } from "../message-cards";
import type { MonacoAppearance } from "../monaco";
import type { SourceRenderableRegion } from "../source-renderers";

export function composeInlineRegionFence(region: SourceRenderableRegion): string {
  if (region.renderer === "mermaid") {
    return "```mermaid\n" + region.displayText.replace(/\s+$/, "") + "\n```";
  }
  if (region.renderer === "math") {
    return "$$\n" + region.displayText.replace(/\s+$/, "") + "\n$$";
  }
  return region.displayText;
}

// Preview pane for source files that have at least one renderable
// region (Phase 3 of `docs/features/source-renderers.md`). For
// Markdown files, delegates to `MarkdownDocumentView` so all the
// existing Markdown chrome (headings, table-of-contents, link
// handling) stays intact. For non-Markdown files the detected
// regions are composed into a synthetic Markdown fragment that
// `MarkdownContent` already knows how to render — reuses the
// Mermaid / KaTeX paths already wired in Phase 1 without a second
// renderer implementation.
//
// Layout rules:
//
// - Dedicated whole-file renderers (e.g. `.mmd` files) compose to a
//   single fence spanning the whole file; `MarkdownContent` picks it
//   up via the Mermaid code-block branch.
// - Mixed-content files (hypothetical — Rust in Phase 5) interleave
//   recognized regions with plain text showing the intervening
//   source; this Phase 3 implementation keeps it simple and shows
//   only the recognized regions with small source line headers so
//   the user can cross-reference them against the Monaco editor in
//   Split mode.
export function RendererPreviewPane({
  appearance,
  content,
  documentPath,
  isMarkdownSource,
  onOpenSourceLink,
  renderableRegions,
  workspaceRoot,
}: {
  appearance: MonacoAppearance;
  content: string;
  documentPath: string;
  isMarkdownSource: boolean;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  renderableRegions: SourceRenderableRegion[];
  workspaceRoot: string | null;
}) {
  if (isMarkdownSource) {
    return (
      <MarkdownDocumentView
        appearance={appearance}
        documentPath={documentPath}
        markdown={content}
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot={workspaceRoot}
      />
    );
  }
  // Non-Markdown files: compose the renderable regions into a
  // synthetic Markdown fragment. Each region becomes either a fenced
  // block (Mermaid / math fence) or an `$$...$$` math block,
  // prefaced by a subtle "Lines X-Y" label so the user can navigate
  // back to Monaco. `MarkdownContent` handles the rendering safely
  // (sandboxed Mermaid iframe, KaTeX output with
  // contentEditable={false} + data-markdown-serialization="skip").
  const synthetic = composeRendererPreviewMarkdown(renderableRegions);
  return (
    <div className="source-renderer-preview" aria-label="Rendered preview">
      <MarkdownContent
        appearance={appearance}
        documentPath={documentPath}
        markdown={synthetic}
        onOpenSourceLink={onOpenSourceLink}
        workspaceRoot={workspaceRoot}
      />
    </div>
  );
}

export function composeRendererPreviewMarkdown(
  regions: SourceRenderableRegion[],
): string {
  if (regions.length === 0) {
    return "";
  }
  return regions
    .map((region) => {
      const header = `**Lines ${region.sourceStartLine}–${region.sourceEndLine}**`;
      const body = composeRendererPreviewRegion(region);
      return `${header}\n\n${body}`;
    })
    .join("\n\n");
}

export function composeRendererPreviewRegion(region: SourceRenderableRegion): string {
  if (region.renderer === "mermaid") {
    return ["```mermaid", region.displayText.replace(/\s+$/, ""), "```"].join("\n");
  }
  if (region.renderer === "math") {
    const trimmed = region.displayText.replace(/\s+$/, "");
    // Block math rendered via `$$...$$` so `remark-math` tokenizes it
    // without the code-block path. Inline math would be awkward in a
    // preview pane, so we promote inline regions to block form too.
    return `$$\n${trimmed}\n$$`;
  }
  // Markdown-renderer region (Phase 5 Rust doc comments) lands here
  // as prose — emit the body directly so `MarkdownContent` parses it.
  return region.displayText;
}

export function describeRenderableKinds(regions: SourceRenderableRegion[]): string {
  if (regions.length === 0) {
    return "Document";
  }
  const kinds = new Set<string>();
  for (const region of regions) {
    if (region.renderer === "mermaid") {
      kinds.add("Mermaid");
    } else if (region.renderer === "math") {
      kinds.add("Math");
    } else if (region.renderer === "markdown") {
      kinds.add("Markdown");
    }
  }
  const ordered = Array.from(kinds).sort();
  return ordered.join(" + ");
}
