import type { DiffMessage } from "./types";

const OMITTED_SECTION_MARKER = "...";
const HUNK_HEADER_PATTERN =
  /^@@ -(?<oldStart>\d+)(?:,(?<oldCount>\d+))? \+(?<newStart>\d+)(?:,(?<newCount>\d+))? @@/;
const TOKEN_PATTERN = /(\s+|[A-Za-z0-9_]+|[^A-Za-z0-9_\s]+)/g;

type DiffHunk = {
  modifiedStart: number;
  lines: string[];
};

export type DiffPreviewHighlight = {
  end: number;
  start: number;
};

export type DiffPreviewCell = {
  highlights: DiffPreviewHighlight[];
  lineNumber: number | null;
  text: string;
};

export type DiffPreviewRow = {
  kind: "added" | "changed" | "context" | "omitted" | "removed";
  left: DiffPreviewCell;
  right: DiffPreviewCell;
};

export type DiffPreviewHunk = {
  header: string | null;
  newCount: number | null;
  newStart: number | null;
  oldCount: number | null;
  oldStart: number | null;
  rows: DiffPreviewRow[];
};

export type DiffPreviewChangeSummary = {
  addedLineCount: number;
  changedLineCount: number;
  removedLineCount: number;
};

export type DiffPreviewModel = {
  changeSummary: DiffPreviewChangeSummary;
  hasStructuredPreview: boolean;
  hunks: DiffPreviewHunk[];
  modifiedText: string;
  note: string | null;
  originalText: string;
};

type DiffPreviewLine = {
  lineNumber: number | null;
  text: string;
};

export function buildDiffPreviewModel(
  diff: string,
  changeType: DiffMessage["changeType"],
  latestFileContent?: string | null,
): DiffPreviewModel {
  const patchPreview = buildPatchPreviewModel(diff, changeType);
  const expandedPreview =
    latestFileContent != null
      ? expandDiffPreviewToWholeFile(
          diff,
          changeType,
          latestFileContent,
          patchPreview.changeSummary,
        )
      : null;

  if (!expandedPreview) {
    return patchPreview;
  }

  return {
    ...patchPreview,
    hasStructuredPreview: patchPreview.hasStructuredPreview || expandedPreview.hasStructuredPreview,
    modifiedText: expandedPreview.modifiedText,
    note: expandedPreview.note,
    originalText: expandedPreview.originalText,
  };
}

function buildPatchPreviewModel(
  diff: string,
  changeType: DiffMessage["changeType"],
): DiffPreviewModel {
  const hunks: DiffPreviewHunk[] = [];
  const lines = diff.split("\n");
  let currentHunk: DiffPreviewHunk | null = null;
  let oldLineNumber: number | null = null;
  let newLineNumber: number | null = null;
  let sawHunkHeader = false;
  let sawOmittedContext = false;
  let pendingAddedLines: DiffPreviewLine[] = [];
  let pendingRemovedLines: DiffPreviewLine[] = [];
  let addedLineCount = 0;
  let changedLineCount = 0;
  let removedLineCount = 0;

  function ensureCurrentHunk() {
    if (currentHunk) {
      return currentHunk;
    }

    currentHunk = {
      header: null,
      newCount: null,
      newStart: null,
      oldCount: null,
      oldStart: null,
      rows: [],
    };
    hunks.push(currentHunk);
    return currentHunk;
  }

  function flushChangeBlock() {
    if (pendingAddedLines.length === 0 && pendingRemovedLines.length === 0) {
      return;
    }

    const hunk = ensureCurrentHunk();
    const pairedLineCount = Math.min(pendingAddedLines.length, pendingRemovedLines.length);
    changedLineCount += pairedLineCount;
    addedLineCount += Math.max(0, pendingAddedLines.length - pairedLineCount);
    removedLineCount += Math.max(0, pendingRemovedLines.length - pairedLineCount);

    for (let index = 0; index < pairedLineCount; index += 1) {
      const left = pendingRemovedLines[index];
      const right = pendingAddedLines[index];
      const highlights = computeChangedLineHighlights(left.text, right.text);
      hunk.rows.push({
        kind: "changed",
        left: {
          highlights: highlights.left,
          lineNumber: left.lineNumber,
          text: left.text,
        },
        right: {
          highlights: highlights.right,
          lineNumber: right.lineNumber,
          text: right.text,
        },
      });
    }

    for (const line of pendingRemovedLines.slice(pairedLineCount)) {
      hunk.rows.push({
        kind: "removed",
        left: {
          highlights: [],
          lineNumber: line.lineNumber,
          text: line.text,
        },
        right: {
          highlights: [],
          lineNumber: null,
          text: "",
        },
      });
    }

    for (const line of pendingAddedLines.slice(pairedLineCount)) {
      hunk.rows.push({
        kind: "added",
        left: {
          highlights: [],
          lineNumber: null,
          text: "",
        },
        right: {
          highlights: [],
          lineNumber: line.lineNumber,
          text: line.text,
        },
      });
    }

    pendingAddedLines = [];
    pendingRemovedLines = [];
  }

  function appendOmittedMarker() {
    if (!currentHunk || currentHunk.rows.length === 0) {
      return;
    }

    currentHunk.rows.push({
      kind: "omitted",
      left: {
        highlights: [],
        lineNumber: null,
        text: OMITTED_SECTION_MARKER,
      },
      right: {
        highlights: [],
        lineNumber: null,
        text: OMITTED_SECTION_MARKER,
      },
    });
    sawOmittedContext = true;
  }

  for (const line of lines) {
    if (line.startsWith("@@")) {
      flushChangeBlock();
      appendOmittedMarker();
      currentHunk = createHunkFromHeader(line);
      hunks.push(currentHunk);
      oldLineNumber = currentHunk.oldStart;
      newLineNumber = currentHunk.newStart;
      sawHunkHeader = true;
      continue;
    }

    if (isDiffMetadataLine(line)) {
      flushChangeBlock();
      continue;
    }

    if (line === "\\ No newline at end of file") {
      flushChangeBlock();
      continue;
    }

    if (line.startsWith("-")) {
      ensureCurrentHunk();
      pendingRemovedLines.push({
        lineNumber: oldLineNumber,
        text: line.slice(1),
      });
      oldLineNumber = incrementLineNumber(oldLineNumber);
      continue;
    }

    if (line.startsWith("+")) {
      ensureCurrentHunk();
      pendingAddedLines.push({
        lineNumber: newLineNumber,
        text: line.slice(1),
      });
      newLineNumber = incrementLineNumber(newLineNumber);
      continue;
    }

    if (line.startsWith(" ")) {
      flushChangeBlock();
      const hunk = ensureCurrentHunk();
      const text = line.slice(1);
      hunk.rows.push({
        kind: "context",
        left: {
          highlights: [],
          lineNumber: oldLineNumber,
          text,
        },
        right: {
          highlights: [],
          lineNumber: newLineNumber,
          text,
        },
      });
      oldLineNumber = incrementLineNumber(oldLineNumber);
      newLineNumber = incrementLineNumber(newLineNumber);
    }
  }

  flushChangeBlock();

  const hasStructuredPreview = hunks.some((hunk) => hunk.rows.length > 0);
  const originalText = changeType === "create" ? "" : flattenPreviewText(hunks, "left");
  const modifiedText = flattenPreviewText(hunks, "right");

  return {
    changeSummary: {
      addedLineCount,
      changedLineCount,
      removedLineCount,
    },
    hasStructuredPreview,
    hunks,
    modifiedText,
    note:
      hasStructuredPreview && (sawOmittedContext || sawHunkHeader)
        ? "Preview reconstructed from the patch. Unchanged regions outside shown hunks are omitted."
        : null,
    originalText,
  };
}

function expandDiffPreviewToWholeFile(
  diff: string,
  changeType: DiffMessage["changeType"],
  latestFileContent: string,
  changeSummary: DiffPreviewChangeSummary,
): Omit<DiffPreviewModel, "hunks"> | null {
  const normalizedLatestFileContent = normalizeLineEndings(latestFileContent);
  if (changeType === "create") {
    return {
      changeSummary,
      hasStructuredPreview: true,
      modifiedText: normalizedLatestFileContent,
      note: null,
      originalText: "",
    };
  }

  const parsedHunks = parseUnifiedDiffHunks(diff);
  if (parsedHunks.length === 0) {
    return null;
  }

  const modifiedSourceLines = splitContentLines(normalizedLatestFileContent);
  const originalLines: string[] = [];
  const modifiedLines: string[] = [];
  let modifiedIndex = 0;

  for (const hunk of parsedHunks) {
    const targetIndex = clampLineIndex(
      hunk.modifiedStart - 1,
      modifiedIndex,
      modifiedSourceLines.length,
    );
    while (modifiedIndex < targetIndex) {
      const line = modifiedSourceLines[modifiedIndex] ?? "";
      originalLines.push(line);
      modifiedLines.push(line);
      modifiedIndex += 1;
    }

    for (const line of hunk.lines) {
      if (line === "\\ No newline at end of file") {
        continue;
      }

      const prefix = line[0] ?? "";
      const content = line.slice(1);
      if (prefix === " ") {
        if (modifiedSourceLines[modifiedIndex] !== content) {
          return null;
        }

        originalLines.push(content);
        modifiedLines.push(content);
        modifiedIndex += 1;
        continue;
      }

      if (prefix === "+") {
        if (modifiedSourceLines[modifiedIndex] !== content) {
          return null;
        }

        modifiedLines.push(content);
        modifiedIndex += 1;
        continue;
      }

      if (prefix === "-") {
        originalLines.push(content);
      }
    }
  }

  while (modifiedIndex < modifiedSourceLines.length) {
    const line = modifiedSourceLines[modifiedIndex] ?? "";
    originalLines.push(line);
    modifiedLines.push(line);
    modifiedIndex += 1;
  }

  return {
    changeSummary,
    hasStructuredPreview: true,
    modifiedText: modifiedLines.join("\n"),
    note: null,
    originalText: originalLines.join("\n"),
  };
}

function parseUnifiedDiffHunks(diff: string): DiffHunk[] {
  const hunks: DiffHunk[] = [];
  let currentHunk: DiffHunk | null = null;

  for (const line of diff.split("\n")) {
    if (line.startsWith("@@")) {
      const hunkHeader = parseExpandedHunkHeader(line);
      if (!hunkHeader) {
        continue;
      }

      currentHunk = {
        modifiedStart: hunkHeader.modifiedStart,
        lines: [],
      };
      hunks.push(currentHunk);
      continue;
    }

    if (!currentHunk) {
      continue;
    }

    if (isDiffMetadataLine(line)) {
      continue;
    }

    if (
      line.startsWith(" ") ||
      line.startsWith("+") ||
      line.startsWith("-") ||
      line === "\\ No newline at end of file"
    ) {
      currentHunk.lines.push(line);
    }
  }

  return hunks;
}

function parseExpandedHunkHeader(line: string) {
  const match = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
  if (!match) {
    return null;
  }

  return {
    modifiedStart: Number.parseInt(match[1] ?? "1", 10),
  };
}

function createHunkFromHeader(header: string): DiffPreviewHunk {
  const match = header.match(HUNK_HEADER_PATTERN);
  const groups = match?.groups;
  const oldStart = groups?.oldStart ? Number(groups.oldStart) : null;
  const newStart = groups?.newStart ? Number(groups.newStart) : null;

  return {
    header,
    newCount: parseHunkCount(groups?.newCount, newStart),
    newStart,
    oldCount: parseHunkCount(groups?.oldCount, oldStart),
    oldStart,
    rows: [],
  };
}

function parseHunkCount(count: string | undefined, start: number | null) {
  if (count) {
    return Number(count);
  }

  if (start === null) {
    return null;
  }

  return 1;
}

function incrementLineNumber(value: number | null) {
  return value === null ? null : value + 1;
}

function flattenPreviewText(hunks: DiffPreviewHunk[], side: "left" | "right") {
  const lines: string[] = [];

  for (const hunk of hunks) {
    for (const row of hunk.rows) {
      if (row.kind === "omitted") {
        lines.push(OMITTED_SECTION_MARKER);
        continue;
      }

      const cell = side === "left" ? row.left : row.right;
      if (cell.text.length > 0) {
        lines.push(cell.text);
      }
    }
  }

  return lines.join("\n");
}

function computeChangedLineHighlights(left: string, right: string) {
  const leftTokens = tokenizeWithPositions(left);
  const rightTokens = tokenizeWithPositions(right);

  let prefixLength = 0;
  while (
    prefixLength < leftTokens.length &&
    prefixLength < rightTokens.length &&
    leftTokens[prefixLength].value === rightTokens[prefixLength].value
  ) {
    prefixLength += 1;
  }

  let leftSuffixLength = leftTokens.length - 1;
  let rightSuffixLength = rightTokens.length - 1;
  while (
    leftSuffixLength >= prefixLength &&
    rightSuffixLength >= prefixLength &&
    leftTokens[leftSuffixLength].value === rightTokens[rightSuffixLength].value
  ) {
    leftSuffixLength -= 1;
    rightSuffixLength -= 1;
  }

  return {
    left: buildHighlightRange(left, leftTokens, prefixLength, leftSuffixLength),
    right: buildHighlightRange(right, rightTokens, prefixLength, rightSuffixLength),
  };
}

function tokenizeWithPositions(text: string) {
  const tokens: Array<{ end: number; start: number; value: string }> = [];
  for (const match of text.matchAll(TOKEN_PATTERN)) {
    const value = match[0];
    const start = match.index ?? 0;
    tokens.push({
      end: start + value.length,
      start,
      value,
    });
  }

  if (tokens.length === 0 && text.length > 0) {
    tokens.push({
      end: text.length,
      start: 0,
      value: text,
    });
  }

  return tokens;
}

function buildHighlightRange(
  text: string,
  tokens: Array<{ end: number; start: number; value: string }>,
  prefixLength: number,
  suffixIndex: number,
): DiffPreviewHighlight[] {
  if (text.length === 0 || tokens.length === 0) {
    return [];
  }

  if (prefixLength >= tokens.length || prefixLength > suffixIndex) {
    if (prefixLength === tokens.length) {
      return [];
    }

    return [
      {
        end: text.length,
        start: tokens[prefixLength]?.start ?? 0,
      },
    ];
  }

  return [
    {
      end: tokens[suffixIndex]?.end ?? text.length,
      start: tokens[prefixLength]?.start ?? 0,
    },
  ];
}

function isDiffMetadataLine(line: string) {
  return (
    line.startsWith("diff --git ") ||
    line.startsWith("index ") ||
    line.startsWith("--- ") ||
    line.startsWith("+++ ") ||
    line.startsWith("new file mode ") ||
    line.startsWith("deleted file mode ") ||
    line.startsWith("rename from ") ||
    line.startsWith("rename to ")
  );
}

function splitContentLines(text: string) {
  if (!text) {
    return [];
  }

  const lines = text.split("\n");
  if (text.endsWith("\n")) {
    lines.pop();
  }
  return lines;
}

function normalizeLineEndings(text: string) {
  return text.replace(/\r\n/g, "\n");
}

function clampLineIndex(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}
