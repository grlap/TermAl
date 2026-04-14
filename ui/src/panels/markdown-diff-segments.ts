import type { GitDiffDocumentSideSource } from "../api";
import type { buildDiffPreviewModel } from "../diff-preview";

export type MarkdownDocumentCompleteness = "full" | "patch";
export type MarkdownDiffPreviewSideSource = GitDiffDocumentSideSource | "patch";

export type MarkdownDiffPreviewSide = {
  completeness: MarkdownDocumentCompleteness;
  content: string;
  note: string | null;
  source: MarkdownDiffPreviewSideSource;
};

export type MarkdownDiffPreviewModel = {
  after: MarkdownDiffPreviewSide;
  before: MarkdownDiffPreviewSide;
};

export type MarkdownDiffDocumentSegment = {
  afterEndOffset: number;
  afterStartOffset: number;
  id: string;
  isInAfterDocument: boolean;
  kind: "added" | "normal" | "removed";
  markdown: string;
  newStart: number | null;
  oldStart: number | null;
};

export type MarkdownDocumentLine = {
  compareText: string;
  end: number;
  identityText: string;
  start: number;
  text: string;
};

type DiffPreviewModel = ReturnType<typeof buildDiffPreviewModel>;

const MARKDOWN_LINE_DIFF_LCS_CELL_LIMIT = 1_000_000;

export function buildMarkdownDiffDocumentSegments(
  markdownPreview: MarkdownDiffPreviewModel,
  preview: DiffPreviewModel,
): MarkdownDiffDocumentSegment[] {
  if (markdownPreview.after.completeness === "full") {
    return buildFullMarkdownDiffDocumentSegments(
      markdownPreview.before.content,
      markdownPreview.after.content,
    );
  }

  return buildPatchMarkdownDiffDocumentSegments(preview);
}

export function buildFullMarkdownDiffDocumentSegments(
  beforeContent: string,
  afterContent: string,
): MarkdownDiffDocumentSegment[] {
  const beforeLines = splitMarkdownDocumentLinesWithOffsets(beforeContent);
  const afterLines = splitMarkdownDocumentLinesWithOffsets(afterContent);
  const anchors = buildMarkdownLineDiffAnchors(beforeLines, afterLines);
  const segments: MarkdownDiffDocumentSegment[] = [];
  let beforeCursor = 0;
  let afterCursor = 0;

  const pushChangedRange = (beforeEnd: number, afterEnd: number) => {
    if (beforeCursor < beforeEnd) {
      const insertionOffset = afterLines[afterCursor]?.start ?? afterContent.length;
      pushMarkdownDiffSegment(segments, {
        afterEndOffset: insertionOffset,
        afterStartOffset: insertionOffset,
        id: `removed:${segments.length}:${beforeCursor}:${beforeEnd}:${afterCursor}`,
        isInAfterDocument: false,
        kind: "removed",
        markdown: beforeLines.slice(beforeCursor, beforeEnd).map((line) => line.text).join(""),
        newStart: afterCursor + 1,
        oldStart: beforeCursor + 1,
      });
    }

    if (afterCursor < afterEnd) {
      const startOffset = afterLines[afterCursor]?.start ?? afterContent.length;
      const endOffset = afterLines[afterEnd - 1]?.end ?? startOffset;
      pushMarkdownDiffSegment(segments, {
        afterEndOffset: endOffset,
        afterStartOffset: startOffset,
        id: `added:${segments.length}:${beforeCursor}:${afterCursor}:${afterEnd}`,
        isInAfterDocument: true,
        kind: "added",
        markdown: afterLines.slice(afterCursor, afterEnd).map((line) => line.text).join(""),
        newStart: afterCursor + 1,
        oldStart: beforeCursor + 1,
      });
    }
  };

  for (const anchor of anchors) {
    pushChangedRange(anchor.beforeIndex, anchor.afterIndex);

    const beforeLine = beforeLines[anchor.beforeIndex];
    const afterLine = afterLines[anchor.afterIndex];
    if (beforeLine && afterLine && beforeLine.identityText !== afterLine.identityText) {
      pushChangedRange(anchor.beforeIndex + 1, anchor.afterIndex + 1);
    } else if (afterLine) {
      pushMarkdownDiffSegment(segments, {
        afterEndOffset: afterLine.end,
        afterStartOffset: afterLine.start,
        id: `normal:${segments.length}:${anchor.beforeIndex}:${anchor.afterIndex}`,
        isInAfterDocument: true,
        kind: "normal",
        markdown: afterLine.text,
        newStart: anchor.afterIndex + 1,
        oldStart: anchor.beforeIndex + 1,
      });
    }

    beforeCursor = anchor.beforeIndex + 1;
    afterCursor = anchor.afterIndex + 1;
  }

  pushChangedRange(beforeLines.length, afterLines.length);

  return segments;
}

export function buildPatchMarkdownDiffDocumentSegments(
  preview: DiffPreviewModel,
): MarkdownDiffDocumentSegment[] {
  const segments: MarkdownDiffDocumentSegment[] = [];

  for (const hunk of preview.hunks) {
    let removedLines: string[] = [];
    let addedLines: string[] = [];
    let oldStart: number | null = null;
    let newStart: number | null = null;

    const flush = () => {
      if (removedLines.length > 0) {
        pushMarkdownDiffSegment(segments, {
          afterEndOffset: 0,
          afterStartOffset: 0,
          id: `removed:${segments.length}:${oldStart ?? "none"}:${newStart ?? "none"}`,
          isInAfterDocument: false,
          kind: "removed",
          markdown: joinMarkdownDiffLines(removedLines),
          newStart,
          oldStart,
        });
      }

      if (addedLines.length > 0) {
        pushMarkdownDiffSegment(segments, {
          afterEndOffset: 0,
          afterStartOffset: 0,
          id: `added:${segments.length}:${oldStart ?? "none"}:${newStart ?? "none"}`,
          isInAfterDocument: true,
          kind: "added",
          markdown: joinMarkdownDiffLines(addedLines),
          newStart,
          oldStart,
        });
      }

      removedLines = [];
      addedLines = [];
      oldStart = null;
      newStart = null;
    };

    for (const row of hunk.rows) {
      if (row.kind === "context") {
        flush();
        pushMarkdownDiffSegment(segments, {
          afterEndOffset: 0,
          afterStartOffset: 0,
          id: `normal:${segments.length}:${row.right.lineNumber ?? "none"}`,
          isInAfterDocument: true,
          kind: "normal",
          markdown: joinMarkdownDiffLines([row.right.text]),
          newStart: row.right.lineNumber,
          oldStart: row.left.lineNumber,
        });
        continue;
      }

      if (row.kind === "omitted") {
        flush();
        pushMarkdownDiffSegment(segments, {
          afterEndOffset: 0,
          afterStartOffset: 0,
          id: `normal:${segments.length}:omitted`,
          isInAfterDocument: false,
          kind: "normal",
          markdown: "...\n",
          newStart: null,
          oldStart: null,
        });
        continue;
      }

      if (row.kind === "removed" || row.kind === "changed") {
        if (oldStart == null) {
          oldStart = row.left.lineNumber;
        }
        removedLines.push(row.left.text);
      }

      if (row.kind === "added" || row.kind === "changed") {
        if (newStart == null) {
          newStart = row.right.lineNumber;
        }
        addedLines.push(row.right.text);
      }
    }

    flush();
  }

  return segments;
}

export function replaceMarkdownDocumentRange(
  content: string,
  startOffset: number,
  endOffset: number,
  replacement: string,
) {
  const start = Math.max(0, Math.min(startOffset, content.length));
  const end = Math.max(start, Math.min(endOffset, content.length));
  return `${content.slice(0, start)}${replacement}${content.slice(end)}`;
}

export function normalizeEditedMarkdownSection(nextMarkdown: string, originalMarkdown: string) {
  let normalized = nextMarkdown.replace(/\u00a0/g, " ");
  if (originalMarkdown.endsWith("\n") && !normalized.endsWith("\n")) {
    normalized += "\n";
  }
  if (!originalMarkdown.endsWith("\n")) {
    normalized = normalized.replace(/\n+$/g, "");
  }
  return normalized;
}

export function splitMarkdownDocumentLinesWithOffsets(content: string): MarkdownDocumentLine[] {
  const matches = content.matchAll(/[^\n]*\n|[^\n]+/g);
  return Array.from(matches, (match) => {
    const start = match.index ?? 0;
    const text = match[0];
    return {
      compareText: normalizeMarkdownLineForDiff(text),
      end: start + text.length,
      identityText: normalizeMarkdownLineEndingsForDiff(text),
      start,
      text,
    };
  });
}

export function buildMarkdownLineDiffAnchors(
  beforeLines: MarkdownDocumentLine[],
  afterLines: MarkdownDocumentLine[],
) {
  return buildMarkdownLineDiffAnchorsInRange(
    beforeLines,
    afterLines,
    0,
    beforeLines.length,
    0,
    afterLines.length,
  );
}

function buildMarkdownLineDiffAnchorsInRange(
  beforeLines: MarkdownDocumentLine[],
  afterLines: MarkdownDocumentLine[],
  beforeStart: number,
  beforeEnd: number,
  afterStart: number,
  afterEnd: number,
): Array<{ afterIndex: number; beforeIndex: number }> {
  const anchors: Array<{ afterIndex: number; beforeIndex: number }> = [];
  const suffixAnchors: Array<{ afterIndex: number; beforeIndex: number }> = [];
  let beforeCursor = beforeStart;
  let afterCursor = afterStart;
  let beforeLimit = beforeEnd;
  let afterLimit = afterEnd;

  while (
    beforeCursor < beforeLimit &&
    afterCursor < afterLimit &&
    beforeLines[beforeCursor]?.compareText === afterLines[afterCursor]?.compareText
  ) {
    anchors.push({ beforeIndex: beforeCursor, afterIndex: afterCursor });
    beforeCursor += 1;
    afterCursor += 1;
  }

  while (
    beforeCursor < beforeLimit &&
    afterCursor < afterLimit &&
    beforeLines[beforeLimit - 1]?.compareText === afterLines[afterLimit - 1]?.compareText
  ) {
    beforeLimit -= 1;
    afterLimit -= 1;
    suffixAnchors.push({ beforeIndex: beforeLimit, afterIndex: afterLimit });
  }
  suffixAnchors.reverse();

  if (beforeCursor >= beforeLimit || afterCursor >= afterLimit) {
    return anchors.concat(suffixAnchors);
  }

  const beforeRangeLength = beforeLimit - beforeCursor;
  const afterRangeLength = afterLimit - afterCursor;
  if ((beforeRangeLength + 1) * (afterRangeLength + 1) <= MARKDOWN_LINE_DIFF_LCS_CELL_LIMIT) {
    return anchors.concat(
      buildMarkdownLineDiffAnchorsWithLcs(
        beforeLines,
        afterLines,
        beforeCursor,
        beforeLimit,
        afterCursor,
        afterLimit,
      ),
      suffixAnchors,
    );
  }

  const uniqueAnchors = buildPatienceMarkdownLineAnchors(
    beforeLines,
    afterLines,
    beforeCursor,
    beforeLimit,
    afterCursor,
    afterLimit,
  );

  if (uniqueAnchors.length === 0) {
    return anchors.concat(suffixAnchors);
  }

  let previousBeforeIndex = beforeCursor;
  let previousAfterIndex = afterCursor;
  for (const anchor of uniqueAnchors) {
    anchors.push(
      ...buildMarkdownLineDiffAnchorsInRange(
        beforeLines,
        afterLines,
        previousBeforeIndex,
        anchor.beforeIndex,
        previousAfterIndex,
        anchor.afterIndex,
      ),
    );
    anchors.push(anchor);
    previousBeforeIndex = anchor.beforeIndex + 1;
    previousAfterIndex = anchor.afterIndex + 1;
  }

  anchors.push(
    ...buildMarkdownLineDiffAnchorsInRange(
      beforeLines,
      afterLines,
      previousBeforeIndex,
      beforeLimit,
      previousAfterIndex,
      afterLimit,
    ),
    ...suffixAnchors,
  );

  return anchors;
}

function buildMarkdownLineDiffAnchorsWithLcs(
  beforeLines: MarkdownDocumentLine[],
  afterLines: MarkdownDocumentLine[],
  beforeStart: number,
  beforeEnd: number,
  afterStart: number,
  afterEnd: number,
) {
  const beforeRangeLength = beforeEnd - beforeStart;
  const afterRangeLength = afterEnd - afterStart;
  const rowCount = beforeRangeLength + 1;
  const columnCount = afterRangeLength + 1;
  const lengths = new Uint32Array(rowCount * columnCount);
  const offset = (row: number, column: number) => row * columnCount + column;
  for (let row = beforeRangeLength - 1; row >= 0; row -= 1) {
    for (let column = afterRangeLength - 1; column >= 0; column -= 1) {
      lengths[offset(row, column)] =
        beforeLines[beforeStart + row]?.compareText === afterLines[afterStart + column]?.compareText
          ? lengths[offset(row + 1, column + 1)] + 1
          : Math.max(lengths[offset(row + 1, column)], lengths[offset(row, column + 1)]);
    }
  }

  const anchors: Array<{ afterIndex: number; beforeIndex: number }> = [];
  let beforeIndex = 0;
  let afterIndex = 0;
  while (beforeIndex < beforeRangeLength && afterIndex < afterRangeLength) {
    if (beforeLines[beforeStart + beforeIndex]?.compareText === afterLines[afterStart + afterIndex]?.compareText) {
      anchors.push({
        beforeIndex: beforeStart + beforeIndex,
        afterIndex: afterStart + afterIndex,
      });
      beforeIndex += 1;
      afterIndex += 1;
      continue;
    }

    if (lengths[offset(beforeIndex + 1, afterIndex)] >= lengths[offset(beforeIndex, afterIndex + 1)]) {
      beforeIndex += 1;
    } else {
      afterIndex += 1;
    }
  }

  return anchors;
}

function buildPatienceMarkdownLineAnchors(
  beforeLines: MarkdownDocumentLine[],
  afterLines: MarkdownDocumentLine[],
  beforeStart: number,
  beforeEnd: number,
  afterStart: number,
  afterEnd: number,
) {
  const beforeOccurrences = buildUsefulMarkdownLineOccurrenceMap(beforeLines, beforeStart, beforeEnd);
  const afterOccurrences = buildUsefulMarkdownLineOccurrenceMap(afterLines, afterStart, afterEnd);
  const candidates: Array<{ afterIndex: number; beforeIndex: number }> = [];

  for (let afterIndex = afterStart; afterIndex < afterEnd; afterIndex += 1) {
    const compareText = afterLines[afterIndex]?.compareText;
    if (!isUsefulMarkdownLineDiffAnchor(compareText)) {
      continue;
    }

    const afterOccurrence = afterOccurrences.get(compareText);
    const beforeOccurrence = beforeOccurrences.get(compareText);
    if (afterOccurrence?.count === 1 && beforeOccurrence?.count === 1) {
      candidates.push({
        beforeIndex: beforeOccurrence.index,
        afterIndex,
      });
    }
  }

  return buildIncreasingBeforeIndexSubsequence(candidates);
}

function buildUsefulMarkdownLineOccurrenceMap(
  lines: MarkdownDocumentLine[],
  start: number,
  end: number,
) {
  const occurrences = new Map<string, { count: number; index: number }>();
  for (let index = start; index < end; index += 1) {
    const compareText = lines[index]?.compareText;
    if (!isUsefulMarkdownLineDiffAnchor(compareText)) {
      continue;
    }

    const current = occurrences.get(compareText);
    if (current) {
      current.count += 1;
    } else {
      occurrences.set(compareText, { count: 1, index });
    }
  }
  return occurrences;
}

function buildIncreasingBeforeIndexSubsequence(
  candidates: Array<{ afterIndex: number; beforeIndex: number }>,
) {
  if (candidates.length <= 1) {
    return candidates;
  }

  const previousCandidateIndexes = new Array<number>(candidates.length).fill(-1);
  const tailCandidateIndexes: number[] = [];

  for (let index = 0; index < candidates.length; index += 1) {
    const candidate = candidates[index];
    let low = 0;
    let high = tailCandidateIndexes.length;
    while (low < high) {
      const mid = Math.floor((low + high) / 2);
      const tailCandidate = candidates[tailCandidateIndexes[mid]];
      if (tailCandidate.beforeIndex < candidate.beforeIndex) {
        low = mid + 1;
      } else {
        high = mid;
      }
    }

    if (low > 0) {
      previousCandidateIndexes[index] = tailCandidateIndexes[low - 1];
    }
    tailCandidateIndexes[low] = index;
  }

  const sequence: Array<{ afterIndex: number; beforeIndex: number }> = [];
  let candidateIndex = tailCandidateIndexes[tailCandidateIndexes.length - 1];
  while (candidateIndex >= 0) {
    sequence.push(candidates[candidateIndex]);
    candidateIndex = previousCandidateIndexes[candidateIndex];
  }

  return sequence.reverse();
}

function isUsefulMarkdownLineDiffAnchor(compareText: string | undefined) {
  if (!compareText) {
    return false;
  }

  const compactText = compareText.trim().replace(/\s+/g, "");
  if (!compactText) {
    return false;
  }

  return !/^(\|?---)+\|?$/.test(compactText) && !/^[-*_]{3,}$/.test(compactText);
}

export function buildGreedyMarkdownLineAnchors(
  beforeLines: MarkdownDocumentLine[],
  afterLines: MarkdownDocumentLine[],
) {
  const anchors: Array<{ afterIndex: number; beforeIndex: number }> = [];
  let beforeCursor = 0;
  for (let afterIndex = 0; afterIndex < afterLines.length; afterIndex += 1) {
    const compareText = afterLines[afterIndex]?.compareText;
    if (compareText == null) {
      continue;
    }

    const beforeIndex = beforeLines.findIndex(
      (line, index) => index >= beforeCursor && line.compareText === compareText,
    );
    if (beforeIndex < 0) {
      continue;
    }

    anchors.push({ beforeIndex, afterIndex });
    beforeCursor = beforeIndex + 1;
  }

  return anchors;
}

function pushMarkdownDiffSegment(
  segments: MarkdownDiffDocumentSegment[],
  segment: MarkdownDiffDocumentSegment,
) {
  if (segment.markdown.trim().length === 0 && segment.kind !== "normal") {
    return;
  }

  const previous = segments[segments.length - 1];
  if (previous && previous.kind === segment.kind && previous.isInAfterDocument === segment.isInAfterDocument) {
    previous.markdown += segment.markdown;
    if (previous.isInAfterDocument) {
      previous.afterEndOffset = segment.afterEndOffset;
    }
    return;
  }

  segments.push(segment);
}

function normalizeMarkdownLineForDiff(line: string) {
  const normalizedLineEndings = normalizeMarkdownLineEndingsForDiff(line);
  const normalizedLinks = normalizedLineEndings
    .replace(/\[`([^`]+)`\]\([^)]+\)/g, "`$1`")
    .replace(/(?<!!)\[([^\]]+)\]\([^)]+\)/g, "$1");
  if (isMarkdownTableSeparatorLine(normalizedLinks)) {
    return normalizedLinks.replace(/:?-{3,}:?/g, "---").replace(/\s+/g, "");
  }
  return normalizedLinks;
}

function normalizeMarkdownLineEndingsForDiff(line: string) {
  return line.replace(/\r\n/g, "\n").replace(/\r$/g, "");
}

function isMarkdownTableSeparatorLine(line: string) {
  return /^\s*\|?\s*:?-{3,}:?\s*(?:\|\s*:?-{3,}:?\s*)+\|?\s*$/.test(line);
}

function joinMarkdownDiffLines(lines: string[]) {
  if (lines.length === 0) {
    return "";
  }

  return `${lines.join("\n")}\n`;
}
