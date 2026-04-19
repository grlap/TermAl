// Small leaf helpers used by the diff panel: the `DiffViewMode`
// enumeration + a default picker, a line-number resolver for
// markdown diff segments, a side-source label map, and a generic
// error-message normaliser. None of this knows about React state
// or Monaco — the panel owns those layers and just consumes these
// helpers.
//
// What this file owns:
//   - `DiffViewMode` — the union of view tabs the diff panel can
//     be in: `"all" | "changes" | "markdown" | "rendered" | "edit"
//     | "raw"`.
//   - `defaultDiffViewMode` — picks the initial view mode for a
//     freshly-opened diff preview. Prefers `"markdown"` when the
//     caller asks, otherwise `"all"` when a structured preview is
//     available, otherwise `"edit"` when a file path is known,
//     falling back to `"raw"`.
//   - `getMarkdownDiffSegmentLineNumber` — resolves the line
//     number a `MarkdownDiffDocumentSegment` should display: prefers
//     `oldStart` for `"removed"` segments and `newStart` for the
//     rest, falling through to the opposite side when one is
//     missing.
//   - `formatMarkdownSideSource` — maps a
//     `MarkdownDiffPreviewSideSource` enum value to its display
//     label ("HEAD" / "Index" / "Worktree" / "Empty" / "Patch").
//   - `getErrorMessage` — pulls `.message` off an `Error`, falling
//     back to "The request failed." for non-Error throwables. Used
//     to surface fetch / save failures to the user.
//
// What this file does NOT own:
//   - Any `DiffViewScrollPositions` state or scroll-tracking logic
//     — that stays in `./DiffPanel.tsx` alongside the per-mode
//     scroll restore wiring.
//   - The `MarkdownDiffDocumentSegment` / `MarkdownDiffPreviewSideSource`
//     types themselves — those live in `./markdown-diff-segments`.
//   - View-mode transitions / persistence — DiffPanel's
//     setViewMode flow owns that.
//
// Split out of `ui/src/panels/DiffPanel.tsx`. Same view-mode
// ordering, same side-source label copy, same fallback error
// string.

import type {
  MarkdownDiffDocumentSegment,
  MarkdownDiffPreviewSideSource,
} from "./markdown-diff-segments";

export type DiffViewMode =
  | "all"
  | "changes"
  | "markdown"
  | "rendered"
  | "edit"
  | "raw";

export function defaultDiffViewMode(
  hasStructuredPreview: boolean,
  hasFilePath: boolean,
  preferMarkdownView = false,
): DiffViewMode {
  if (preferMarkdownView) {
    return "markdown";
  }

  if (hasStructuredPreview) {
    return "all";
  }

  return hasFilePath ? "edit" : "raw";
}

export function getMarkdownDiffSegmentLineNumber(segment: MarkdownDiffDocumentSegment) {
  return segment.kind === "removed"
    ? segment.oldStart ?? segment.newStart ?? null
    : segment.newStart ?? segment.oldStart ?? null;
}

export function formatMarkdownSideSource(source: MarkdownDiffPreviewSideSource) {
  switch (source) {
    case "head":
      return "HEAD";
    case "index":
      return "Index";
    case "worktree":
      return "Worktree";
    case "empty":
      return "Empty";
    case "patch":
      return "Patch";
  }
}

export function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The request failed.";
}
