import { useMemo } from "react";
import { DeferredHeavyContent } from "./deferred-heavy-content";
import {
  buildMarkdownPreviewText,
  estimateMarkdownBlockHeight,
  measureTextBlock,
} from "./deferred-render";
import { MarkdownContent } from "./markdown-content";
import type { MarkdownFileLinkTarget } from "./markdown-links";
import type { MonacoAppearance } from "./monaco";
import {
  containsSearchMatch,
  type SearchHighlightTone,
} from "./search-highlight";

// Markdown qualifies as "heavy" - and therefore goes through
// `DeferredMarkdownContent`'s IntersectionObserver-gated render path -
// when EITHER the line count OR the character count crosses these
// thresholds. Both gates are OR'd, so a long single-line table or a
// short-but-very-wide block still qualifies.
//
// Tuning history:
//   - 1800 chars / 24 lines: original conservative gate, sized so that
//     even mid-size assistant explanations (a few paragraphs of prose,
//     no code) deferred. This made fast scrolling feel laggy because
//     ordinary message bodies blanked out and re-painted on dwell.
//   - 9000 chars / 24 lines (current): only genuinely heavy content
//     (long-form docs, large markdown tables, dumped logs in a fenced
//     block, big mermaid sources) goes through the deferred path;
//     typical assistant prose paints synchronously like plain text and
//     never flickers during fast scroll.
//
// Mermaid note: any ```mermaid``` fence inside the body triggers a
// real mermaid render in a sandboxed iframe (`MermaidDiagram` ->
// `renderTermalMermaidDiagram`), capped at
// `MAX_MERMAID_DIAGRAMS_PER_DOCUMENT` (20). The first render of a
// non-trivial flowchart can take hundreds of ms, so messages with
// embedded mermaid are exactly the case the deferred wrapper exists
// to protect - even if the surrounding prose is short, the character
// count of the source fence usually pushes the body past this gate.
//
// If you raise these further, weigh the cost: every non-deferred
// markdown body re-runs the full remark/rehype/KaTeX/highlight pipeline
// on each mount/unmount the virtualizer performs. The deferred wrapper
// exists specifically to keep those pipelines off the hot path while
// the user is still scrolling. The cooldown that gates the deferred
// path between scroll inputs lives in
// `panels/VirtualizedConversationMessageList.tsx` as
// `DEFERRED_HEAVY_ACTIVATION_COOLDOWN_MS` - keep these two tunings in
// mind as a pair.
const HEAVY_MARKDOWN_CHARACTER_THRESHOLD = 9000;
const HEAVY_MARKDOWN_LINE_THRESHOLD = 24;

/*
 * Streaming-stable assistant markdown wrapper.
 *
 * Always returns the same JSX shape - `<DeferredHeavyContent>` wrapping
 * `<MarkdownContent>` - regardless of whether the message is mid-stream
 * or settled, light or heavy, has a search match or not. This is what
 * gives `<MarkdownContent>` a stable React tree position across the
 * streaming -> settled transition and prevents the full subtree remount
 * that previously caused visible flicker (Mermaid diagrams re-rendering,
 * KaTeX nodes re-mounting, code blocks losing scroll position, tables
 * blinking) the moment a turn ended.
 *
 * `isStreaming` is honored two ways:
 *   - It is passed through to `MarkdownContent`, which uses it to gate
 *     the partial-block deferral splitter (`markdown-streaming-split.ts`).
 *   - It forces `preferImmediateRender = true` on the outer
 *     `DeferredHeavyContent`, so streaming content never goes behind
 *     the heavy-content activation gate. The placeholder (used by the
 *     gate when `shouldGate` is true) is correspondingly elided so it
 *     can never appear during streaming.
 *
 * `DeferredHeavyContent`'s `isActivated` state is monotonic (only flips
 * from false -> true), so when a heavy message transitions out of
 * streaming, the wrapper stays activated and content stays visible -
 * the parent's `preferImmediateRender` only matters for the initial
 * mount of a settled heavy bubble.
 */
export function DeferredMarkdownContent({
  appearance = "dark",
  documentPath = null,
  isStreaming = false,
  markdown,
  onOpenSourceLink,
  preferImmediateRender = false,
  searchQuery = "",
  searchHighlightTone = "match",
  workspaceRoot = null,
}: {
  appearance?: MonacoAppearance;
  documentPath?: string | null;
  isStreaming?: boolean;
  markdown: string;
  onOpenSourceLink?: (target: MarkdownFileLinkTarget) => void;
  preferImmediateRender?: boolean;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
  workspaceRoot?: string | null;
}) {
  const metrics = useMemo(() => measureTextBlock(markdown), [markdown]);
  const showSearchHighlight = containsSearchMatch(markdown, searchQuery);
  // Heavy-content activation gate: only engages for settled, large,
  // non-search bubbles. Streaming content always renders immediately
  // (the user is watching it being authored). Search results always
  // render immediately (the highlighted match must be visible).
  const shouldGate =
    !isStreaming &&
    !showSearchHighlight &&
    (metrics.lineCount >= HEAVY_MARKDOWN_LINE_THRESHOLD ||
      markdown.length >= HEAVY_MARKDOWN_CHARACTER_THRESHOLD);
  const effectivePreferImmediateRender = !shouldGate || preferImmediateRender;

  return (
    <DeferredHeavyContent
      estimatedHeight={
        shouldGate ? estimateMarkdownBlockHeight(metrics.lineCount) : 0
      }
      preferImmediateRender={effectivePreferImmediateRender}
      placeholder={
        shouldGate ? (
          <div className="markdown-copy deferred-markdown-placeholder">
            <p className="plain-text-copy">
              {buildMarkdownPreviewText(markdown)}
            </p>
          </div>
        ) : null
      }
    >
      <MarkdownContent
        appearance={appearance}
        documentPath={documentPath}
        isStreaming={isStreaming}
        markdown={markdown}
        onOpenSourceLink={onOpenSourceLink}
        searchQuery={searchQuery}
        searchHighlightTone={searchHighlightTone}
        workspaceRoot={workspaceRoot}
      />
    </DeferredHeavyContent>
  );
}
