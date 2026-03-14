import type { DiffMessage } from "./types";

const OMITTED_SECTION_MARKER = "...";

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

    if (
      line.startsWith("diff --git ") ||
      line.startsWith("index ") ||
      line.startsWith("--- ") ||
      line.startsWith("+++ ")
    ) {
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
      continue;
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

function appendOmittedSectionMarker(originalLines: string[], modifiedLines: string[]) {
  if (originalLines[originalLines.length - 1] !== OMITTED_SECTION_MARKER) {
    originalLines.push(OMITTED_SECTION_MARKER);
  }

  if (modifiedLines[modifiedLines.length - 1] !== OMITTED_SECTION_MARKER) {
    modifiedLines.push(OMITTED_SECTION_MARKER);
  }
}
