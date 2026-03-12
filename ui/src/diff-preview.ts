import type { DiffMessage } from "./types";

const OMITTED_SECTION_MARKER = "...";

export type DiffPreviewModel = {
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

  for (const line of lines) {
    if (line.startsWith("@@")) {
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
      continue;
    }

    if (line === "\\ No newline at end of file") {
      continue;
    }

    if (line.startsWith("-")) {
      originalLines.push(line.slice(1));
      currentHunkHasContent = true;
      continue;
    }

    if (line.startsWith("+")) {
      modifiedLines.push(line.slice(1));
      currentHunkHasContent = true;
      continue;
    }

    if (line.startsWith(" ")) {
      const content = line.slice(1);
      originalLines.push(content);
      modifiedLines.push(content);
      currentHunkHasContent = true;
      continue;
    }
  }

  const originalText = changeType === "create" ? "" : originalLines.join("\n");
  const modifiedText = modifiedLines.join("\n");
  const hasStructuredPreview = originalText.length > 0 || modifiedText.length > 0;

  return {
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
