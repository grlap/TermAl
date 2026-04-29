// Change-block index for the rendered-Markdown diff view. Walks the
// segment list with the SAME grouping rules `renderMarkdownDiffSegments`
// in `./markdown-diff-view` uses, so navigation stops match the visible
// blocks 1:1 (one block per group of consecutive non-`normal` segments,
// with the same `added → removed` break the renderer applies).
//
// What this file owns:
//   - `MarkdownDiffChangeBlock` — the per-block descriptor: a stable
//     `id` that matches the renderer's `key` (segment ids joined by
//     `:`), and the `segments` that compose the block.
//   - `computeMarkdownDiffChangeBlocks(segments)` — pure walk that
//     produces the block list in document order.
//
// What this file does NOT own:
//   - The block JSX, the `<section className="markdown-diff-change-block">`
//     wrapper, or the `data-markdown-diff-change-index` attribute —
//     those live in `./markdown-diff-view::renderMarkdownDiffSegments`.
//   - The current-change-index UI state (prev/next buttons, the
//     `Change X of Y` label, scroll-into-view side effect) — that
//     lives in `./markdown-diff-view::MarkdownDiffView`.
//   - Segment construction / stability — those live in
//     `./markdown-diff-segments` and `./markdown-diff-segment-stability`.
//
// New module added when the Medium-severity bug "Rendered Markdown
// diff view cannot jump between changes" was closed: the index is
// derivable from the same model the renderer uses, so navigation
// stops are always in sync with the rendered blocks (no separate
// DOM walk, no per-block measurement).

import type { MarkdownDiffDocumentSegment } from "./markdown-diff-segments";

export type MarkdownDiffChangeBlock = {
  /**
   * Stable id matching the renderer's `key` for the block: the
   * change-segment ids joined by `:`. Used as the React `key` AND as
   * the lookup target when the navigation UI scrolls a block into
   * view via `data-markdown-diff-change-index="N"`.
   */
  id: string;
  segments: MarkdownDiffDocumentSegment[];
};

/**
 * Walks `segments` in document order and emits one
 * `MarkdownDiffChangeBlock` per group of consecutive non-`normal`
 * segments. The grouping mirrors `renderMarkdownDiffSegments`:
 *   - `normal` segments are skipped entirely (they are not change
 *     blocks).
 *   - Consecutive `added`/`removed` segments collect into one block.
 *   - The collection breaks at an `added → removed` boundary so a
 *     pure-add right before a fence replacement is its own block,
 *     not smeared into the fence's removed→added pair. (Comment in
 *     the renderer explains why; the rule is duplicated here so the
 *     navigation index does not drift from what the user sees.)
 */
export function computeMarkdownDiffChangeBlocks(
  segments: readonly MarkdownDiffDocumentSegment[],
): MarkdownDiffChangeBlock[] {
  const blocks: MarkdownDiffChangeBlock[] = [];
  for (let index = 0; index < segments.length; index += 1) {
    const segment = segments[index];
    if (!segment) {
      continue;
    }
    if (segment.kind === "normal") {
      continue;
    }

    const groupedSegments: MarkdownDiffDocumentSegment[] = [segment];
    while (
      segments[index + 1]?.kind !== "normal" &&
      segments[index + 1] != null
    ) {
      const current = groupedSegments[groupedSegments.length - 1];
      const next = segments[index + 1];
      if (current?.kind === "added" && next?.kind === "removed") {
        break;
      }
      groupedSegments.push(segments[index + 1]!);
      index += 1;
    }

    blocks.push({
      id: groupedSegments.map((entry) => entry.id).join(":"),
      segments: groupedSegments,
    });
  }
  return blocks;
}
