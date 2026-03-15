import type { DiffMessage } from "./types";

const OMITTED_SECTION_MARKER = "...";

type DiffHunk = {
  modifiedStart: number;
  lines: string[];
};

export type DiffPreviewChangeSummary = {
  addedLineCount: number;
  changedLineCount: number;
  removedLineCount: number;
};

export type DiffPreviewModel = {
  changeSummary: DiffPreviewChangeSummary;
  hasStructuredPreview: boolean;
  modifiedText: string;
  note: string | null;
  originalText: string;
};

export function buildDiffPreviewModel(
  diff: string,
  changeType: DiffMessage["changeType"],
  latestFileContent?: string | null,
): DiffPreviewModel {
  const fallbackPreview = buildPatchPreviewModel(diff, changeType);
  const expandedPreview =
    latestFileContent != null
      ? expandDiffPreviewToWholeFile(
          diff,
          changeType,
          latestFileContent,
          fallbackPreview.changeSummary,
        )
      : null;

  return expandedPreview ?? fallbackPreview;
}

function buildPatchPreviewModel(
  diff: string,
  changeType: DiffMessage["changeType"],
): DiffPreviewModel {
  const originalLines: string[] = [];
  const modifiedLines: string[] = [];
  const lines = diff.split("\n");
  let sawOmittedContext = false;
  let currentHunkHasContent = false;
  let sawHunkHeader = false;
  let pendingAddedLineCount = 0;
  let pendingRemovedLineCount = 0;
  let addedLineCount = 0;
  let changedLineCount = 0;
  let removedLineCount = 0;

  function flushChangeBlock() {
    if (pendingAddedLineCount === 0 && pendingRemovedLineCount === 0) {
      return;
    }

    const changedLineDelta = Math.min(pendingAddedLineCount, pendingRemovedLineCount);
    changedLineCount += changedLineDelta;
    addedLineCount += Math.max(0, pendingAddedLineCount - changedLineDelta);
    removedLineCount += Math.max(0, pendingRemovedLineCount - changedLineDelta);
    pendingAddedLineCount = 0;
    pendingRemovedLineCount = 0;
  }

  for (const line of lines) {
    if (line.startsWith("@@")) {
      flushChangeBlock();
      if (sawHunkHeader && currentHunkHasContent) {
        appendOmittedSectionMarker(originalLines, modifiedLines);
        sawOmittedContext = true;
      }
      sawHunkHeader = true;
      currentHunkHasContent = false;
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
      originalLines.push(line.slice(1));
      pendingRemovedLineCount += 1;
      currentHunkHasContent = true;
      continue;
    }

    if (line.startsWith("+")) {
      modifiedLines.push(line.slice(1));
      pendingAddedLineCount += 1;
      currentHunkHasContent = true;
      continue;
    }

    if (line.startsWith(" ")) {
      flushChangeBlock();
      const content = line.slice(1);
      originalLines.push(content);
      modifiedLines.push(content);
      currentHunkHasContent = true;
    }
  }

  flushChangeBlock();

  const originalText = changeType === "create" ? "" : originalLines.join("\n");
  const modifiedText = modifiedLines.join("\n");
  const hasStructuredPreview = originalText.length > 0 || modifiedText.length > 0;

  return {
    changeSummary: {
      addedLineCount,
      changedLineCount,
      removedLineCount,
    },
    hasStructuredPreview,
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
): DiffPreviewModel | null {
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

  const hunks = parseUnifiedDiffHunks(diff);
  if (hunks.length === 0) {
    return null;
  }

  const modifiedSourceLines = splitContentLines(normalizedLatestFileContent);
  const originalLines: string[] = [];
  const modifiedLines: string[] = [];
  let modifiedIndex = 0;

  for (const hunk of hunks) {
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
      const hunkHeader = parseHunkHeader(line);
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

function parseHunkHeader(line: string) {
  const match = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
  if (!match) {
    return null;
  }

  return {
    modifiedStart: Number.parseInt(match[1] ?? "1", 10),
  };
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

function appendOmittedSectionMarker(originalLines: string[], modifiedLines: string[]) {
  if (originalLines[originalLines.length - 1] !== OMITTED_SECTION_MARKER) {
    originalLines.push(OMITTED_SECTION_MARKER);
  }

  if (modifiedLines[modifiedLines.length - 1] !== OMITTED_SECTION_MARKER) {
    modifiedLines.push(OMITTED_SECTION_MARKER);
  }
}
