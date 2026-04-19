// Keep Markdown diff segment ids stable across re-renders so the
// rendered-view React tree does not remount sections whose
// content is unchanged.
//
// What this file owns:
//   - `useStableMarkdownDiffDocumentSegments` — the hook
//     `DiffPanel` uses to feed segments into the rendered-
//     Markdown tree. Holds the previous committed segments in a
//     ref, runs `stabilizeMarkdownDiffSegmentIds` against them on
//     every re-render, and updates the ref after commit.
//   - `stabilizeMarkdownDiffSegmentIds` — given a previous set of
//     segments (with committed ids) and a freshly-computed next
//     set, produces a next set whose segments carry previous ids
//     wherever content + context strongly match. Sorts
//     candidate pairs by context score then distance, greedily
//     matches the best pairs, and falls back to `:new` /
//     `:stable-N` suffixes when the occurrence-based id collides
//     with a reserved one.
//   - Supporting pure helpers: `indexMarkdownDiffSegments`,
//     `getMarkdownDiffSegmentContext`,
//     `getMarkdownDiffSegmentIdentityOrNull`,
//     `getMarkdownDiffSegmentIdentity`,
//     `getStableMarkdownDiffSegmentHash` (FNV-1a 32-bit hash
//     base-36 encoded), `scoreMarkdownDiffSegmentContext`,
//     `scoreMarkdownDiffSegmentContextValue`,
//     `getMarkdownDiffSegmentDistance`,
//     `getAvailableMarkdownDiffSegmentId`.
//   - `IndexedMarkdownDiffSegment` and `MarkdownDiffSegmentContext`
//     types.
//
// What this file does NOT own:
//   - `MarkdownDiffDocumentSegment` itself — defined in
//     `./markdown-diff-segments`. This module adopts that shape
//     and returns the same shape with stabilised ids.
//   - Rendering the segments — the Markdown diff tree in
//     `DiffPanel.tsx` consumes the stabilised segments and
//     renders them.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same function
// bodies, same thresholds (distance weights, context scores, FNV
// hash seed / prime); consumers import from here directly.

import { useEffect, useMemo, useRef } from "react";

import type { MarkdownDiffDocumentSegment } from "./markdown-diff-segments";

export function useStableMarkdownDiffDocumentSegments(
  segments: MarkdownDiffDocumentSegment[],
  identityKey: string,
) {
  const previousRef = useRef<{
    identityKey: string;
    segments: MarkdownDiffDocumentSegment[];
  } | null>(null);
  const stableSegments = useMemo(() => {
    const previous =
      previousRef.current?.identityKey === identityKey
        ? previousRef.current.segments
        : [];
    return stabilizeMarkdownDiffSegmentIds(previous, segments);
  }, [identityKey, segments]);

  useEffect(() => {
    previousRef.current = { identityKey, segments: stableSegments };
  }, [identityKey, stableSegments]);

  return stableSegments;
}

export type IndexedMarkdownDiffSegment = {
  context: MarkdownDiffSegmentContext;
  identity: string;
  index: number;
  segment: MarkdownDiffDocumentSegment;
};

export type MarkdownDiffSegmentContext = {
  next: string | null;
  nextNext: string | null;
  previous: string | null;
  previousPrevious: string | null;
};

export function stabilizeMarkdownDiffSegmentIds(
  previousSegments: MarkdownDiffDocumentSegment[],
  nextSegments: MarkdownDiffDocumentSegment[],
) {
  if (previousSegments.length === 0 || nextSegments.length === 0) {
    return nextSegments;
  }

  const previousEntries = indexMarkdownDiffSegments(previousSegments);
  const nextEntries = indexMarkdownDiffSegments(nextSegments);
  const previousByIdentity = new Map<string, IndexedMarkdownDiffSegment[]>();
  for (const entry of previousEntries) {
    const entries = previousByIdentity.get(entry.identity) ?? [];
    entries.push(entry);
    previousByIdentity.set(entry.identity, entries);
  }

  const candidatePairs: Array<{
    contextScore: number;
    distance: number;
    nextIndex: number;
    previousIndex: number;
  }> = [];
  for (const nextEntry of nextEntries) {
    const matchingPreviousEntries = previousByIdentity.get(nextEntry.identity) ?? [];
    for (const previousEntry of matchingPreviousEntries) {
      candidatePairs.push({
        contextScore: scoreMarkdownDiffSegmentContext(
          previousEntry.context,
          nextEntry.context,
        ),
        distance: getMarkdownDiffSegmentDistance(
          previousEntry.segment,
          nextEntry.segment,
          previousEntry.index,
          nextEntry.index,
        ),
        nextIndex: nextEntry.index,
        previousIndex: previousEntry.index,
      });
    }
  }

  candidatePairs.sort(
    (left, right) =>
      right.contextScore - left.contextScore ||
      left.distance - right.distance ||
      left.nextIndex - right.nextIndex ||
      left.previousIndex - right.previousIndex,
  );

  const matchedPreviousIndexes = new Set<number>();
  const previousIdByNextIndex = new Map<number, string>();
  // Reuse ids from the previous committed render before trusting the new
  // occurrence-based ids. This keeps repeated equal Markdown chunks mounted
  // when an earlier duplicate is inserted or removed.
  for (const pair of candidatePairs) {
    if (
      matchedPreviousIndexes.has(pair.previousIndex) ||
      previousIdByNextIndex.has(pair.nextIndex)
    ) {
      continue;
    }

    const previousSegment = previousSegments[pair.previousIndex];
    if (!previousSegment) {
      continue;
    }

    matchedPreviousIndexes.add(pair.previousIndex);
    previousIdByNextIndex.set(pair.nextIndex, previousSegment.id);
  }

  const usedIds = new Set<string>();
  const reservedMatchedIds = new Set(previousIdByNextIndex.values());
  return nextSegments.map((segment, index) => {
    const matchedPreviousId = previousIdByNextIndex.get(index);
    const preferredId =
      matchedPreviousId ??
      (reservedMatchedIds.has(segment.id) ? `${segment.id}:new` : segment.id);
    const id = getAvailableMarkdownDiffSegmentId(preferredId, usedIds);
    usedIds.add(id);
    return id === segment.id ? segment : { ...segment, id };
  });
}

export function indexMarkdownDiffSegments(segments: MarkdownDiffDocumentSegment[]) {
  return segments.map((segment, index) => ({
    context: getMarkdownDiffSegmentContext(segments, index),
    identity: getMarkdownDiffSegmentIdentity(segment),
    index,
    segment,
  }));
}

export function getMarkdownDiffSegmentContext(
  segments: MarkdownDiffDocumentSegment[],
  index: number,
): MarkdownDiffSegmentContext {
  return {
    next: getMarkdownDiffSegmentIdentityOrNull(segments[index + 1]),
    nextNext: getMarkdownDiffSegmentIdentityOrNull(segments[index + 2]),
    previous: getMarkdownDiffSegmentIdentityOrNull(segments[index - 1]),
    previousPrevious: getMarkdownDiffSegmentIdentityOrNull(segments[index - 2]),
  };
}

export function getMarkdownDiffSegmentIdentityOrNull(
  segment: MarkdownDiffDocumentSegment | undefined,
) {
  return segment ? getMarkdownDiffSegmentIdentity(segment) : null;
}

export function getMarkdownDiffSegmentIdentity(segment: MarkdownDiffDocumentSegment) {
  return [
    segment.kind,
    segment.isInAfterDocument ? "after" : "before",
    segment.markdown.length,
    getStableMarkdownDiffSegmentHash(segment.markdown),
  ].join("\0");
}

export function getStableMarkdownDiffSegmentHash(text: string) {
  let hash = 0x811c9dc5;
  for (let index = 0; index < text.length; index += 1) {
    hash ^= text.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(36);
}

export function scoreMarkdownDiffSegmentContext(
  previousContext: MarkdownDiffSegmentContext,
  nextContext: MarkdownDiffSegmentContext,
) {
  return (
    scoreMarkdownDiffSegmentContextValue(previousContext.previous, nextContext.previous, 8) +
    scoreMarkdownDiffSegmentContextValue(previousContext.next, nextContext.next, 8) +
    scoreMarkdownDiffSegmentContextValue(
      previousContext.previousPrevious,
      nextContext.previousPrevious,
      2,
    ) +
    scoreMarkdownDiffSegmentContextValue(previousContext.nextNext, nextContext.nextNext, 2)
  );
}

export function scoreMarkdownDiffSegmentContextValue(
  previousValue: string | null,
  nextValue: string | null,
  score: number,
) {
  return previousValue != null && previousValue === nextValue ? score : 0;
}

export function getMarkdownDiffSegmentDistance(
  previousSegment: MarkdownDiffDocumentSegment,
  nextSegment: MarkdownDiffDocumentSegment,
  previousIndex: number,
  nextIndex: number,
) {
  const distances = [
    Math.abs(previousIndex - nextIndex) * 1_000_000,
  ];
  if (previousSegment.newStart != null && nextSegment.newStart != null) {
    distances.push(Math.abs(previousSegment.newStart - nextSegment.newStart) * 10_000);
  }
  if (previousSegment.oldStart != null && nextSegment.oldStart != null) {
    distances.push(Math.abs(previousSegment.oldStart - nextSegment.oldStart) * 10_000);
  }
  if (
    previousSegment.afterStartOffset !== 0 ||
    previousSegment.afterEndOffset !== 0 ||
    nextSegment.afterStartOffset !== 0 ||
    nextSegment.afterEndOffset !== 0
  ) {
    distances.push(
      Math.abs(previousSegment.afterStartOffset - nextSegment.afterStartOffset),
    );
  }
  return Math.min(...distances);
}

export function getAvailableMarkdownDiffSegmentId(
  preferredId: string,
  usedIds: Set<string>,
) {
  if (!usedIds.has(preferredId)) {
    return preferredId;
  }

  let suffix = 1;
  while (usedIds.has(`${preferredId}:stable-${suffix}`)) {
    suffix += 1;
  }
  return `${preferredId}:stable-${suffix}`;
}
