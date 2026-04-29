import { describe, expect, it } from "vitest";
import {
  computeMarkdownDiffChangeBlocks,
  type MarkdownDiffChangeBlock,
} from "./markdown-diff-change-index";
import type { MarkdownDiffDocumentSegment } from "./markdown-diff-segments";

function makeSegment(
  id: string,
  kind: MarkdownDiffDocumentSegment["kind"],
): MarkdownDiffDocumentSegment {
  return {
    afterEndOffset: 0,
    afterStartOffset: 0,
    id,
    isInAfterDocument: kind !== "removed",
    kind,
    markdown: id,
    newStart: null,
    oldStart: null,
  };
}

function blockIds(blocks: MarkdownDiffChangeBlock[]) {
  return blocks.map((block) => block.id);
}

describe("computeMarkdownDiffChangeBlocks", () => {
  it("returns no blocks when every segment is unchanged", () => {
    const blocks = computeMarkdownDiffChangeBlocks([
      makeSegment("a", "normal"),
      makeSegment("b", "normal"),
      makeSegment("c", "normal"),
    ]);
    expect(blocks).toHaveLength(0);
  });

  it("emits one block per isolated change between unchanged segments", () => {
    const blocks = computeMarkdownDiffChangeBlocks([
      makeSegment("a", "normal"),
      makeSegment("b", "added"),
      makeSegment("c", "normal"),
      makeSegment("d", "removed"),
      makeSegment("e", "normal"),
    ]);
    expect(blockIds(blocks)).toEqual(["b", "d"]);
  });

  it("groups consecutive removed→added segments into one change block", () => {
    // The renderer collapses a removed→added pair into a single
    // block (red bubble + green bubble side-by-side). Navigation
    // mirrors that — prev/next stop at the GROUPED block, not at
    // each individual segment.
    const blocks = computeMarkdownDiffChangeBlocks([
      makeSegment("a", "normal"),
      makeSegment("b", "removed"),
      makeSegment("c", "added"),
      makeSegment("d", "normal"),
    ]);
    expect(blockIds(blocks)).toEqual(["b:c"]);
    expect(blocks[0]?.segments.map((s) => s.kind)).toEqual([
      "removed",
      "added",
    ]);
  });

  it("breaks the change-block at an added→removed transition", () => {
    // Mirrors the renderer's break-rule: a pure-add right before a
    // fence replacement (added then removed-then-added) should be
    // its own block, not smeared into the fence's pair.
    const blocks = computeMarkdownDiffChangeBlocks([
      makeSegment("normal-1", "normal"),
      makeSegment("added-pre-fence", "added"),
      makeSegment("removed-fence", "removed"),
      makeSegment("added-fence", "added"),
      makeSegment("normal-2", "normal"),
    ]);
    expect(blockIds(blocks)).toEqual([
      "added-pre-fence",
      "removed-fence:added-fence",
    ]);
  });

  it("preserves block id stability across re-runs with the same segments", () => {
    const segments: MarkdownDiffDocumentSegment[] = [
      makeSegment("normal-a", "normal"),
      makeSegment("removed-1", "removed"),
      makeSegment("added-1", "added"),
      makeSegment("normal-b", "normal"),
      makeSegment("added-2", "added"),
    ];
    const first = computeMarkdownDiffChangeBlocks(segments);
    const second = computeMarkdownDiffChangeBlocks(segments);
    expect(blockIds(first)).toEqual(blockIds(second));
    expect(blockIds(first)).toEqual(["removed-1:added-1", "added-2"]);
  });

  it("produces an empty list for an empty segment array", () => {
    expect(computeMarkdownDiffChangeBlocks([])).toEqual([]);
  });

  it("handles a document that ends inside a change block", () => {
    const blocks = computeMarkdownDiffChangeBlocks([
      makeSegment("normal-1", "normal"),
      makeSegment("removed-1", "removed"),
      makeSegment("added-1", "added"),
    ]);
    expect(blockIds(blocks)).toEqual(["removed-1:added-1"]);
  });
});
